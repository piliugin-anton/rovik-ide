//! Evaluation-only native cross-encoder scoring of explicit query/document pairs.
//! Model provisioning and policy selection stay outside this example.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io::Write, path::Path};
use tract_onnx::prelude::*;

type Error = Box<dyn std::error::Error>;
const MAX_TOKENS: usize = 512;

#[derive(Deserialize)]
struct Pair {
    id: String,
    query: String,
    document: String,
}

#[derive(Serialize)]
struct Score<'a> {
    id: &'a str,
    logit: f32,
    tokens: usize,
}

fn validate_pairs(pairs: &[Pair]) -> Result<(), Error> {
    let mut ids = BTreeSet::new();
    for pair in pairs {
        if pair.id.is_empty() || pair.query.trim().is_empty() || pair.document.trim().is_empty() {
            return Err("pair IDs, queries and documents must be nonempty".into());
        }
        if !ids.insert(&pair.id) {
            return Err(format!("duplicate pair ID: {}", pair.id).into());
        }
    }
    Ok(())
}

fn paired_tokens(
    tokenizer: &tokenizers::Tokenizer,
    pair: &Pair,
) -> Result<tokenizers::Encoding, Error> {
    tokenizer
        .encode((pair.query.as_str(), pair.document.as_str()), true)
        .map_err(|e| format!("cannot tokenize pair {}: {e}", pair.id).into())
}

fn classification_logit(output: &Tensor) -> Result<f32, Error> {
    let values = output.to_array_view::<f32>()?;
    if values.shape() != [1, 1] {
        return Err("cross-encoder must return one raw classification logit shaped [1,1]".into());
    }
    let logit = values[[0, 0]];
    if !logit.is_finite() {
        return Err("cross-encoder returned a non-finite logit".into());
    }
    Ok(logit)
}

fn score(directory: &Path, pairs: &[Pair], mut writer: impl Write) -> Result<(), Error> {
    validate_pairs(pairs)?;
    let mut tokenizer = tokenizers::Tokenizer::from_file(directory.join("tokenizer.json"))
        .map_err(|e| format!("cannot load pair tokenizer: {e}"))?;
    tokenizer
        .with_truncation(Some(tokenizers::TruncationParams {
            max_length: MAX_TOKENS,
            strategy: tokenizers::TruncationStrategy::LongestFirst,
            ..Default::default()
        }))
        .map_err(|e| format!("cannot configure pair truncation: {e}"))?;
    tokenizer.with_padding(None);
    let mut model = tract_onnx::onnx().model_for_path(directory.join("model.onnx"))?;
    let names = model
        .input_outlets()?
        .iter()
        .map(|outlet| {
            model
                .outlet_label(*outlet)
                .unwrap_or(&model.node(outlet.node).name)
                .to_owned()
        })
        .collect::<Vec<_>>();
    let required = ["input_ids", "attention_mask", "token_type_ids"]
        .into_iter()
        .collect::<BTreeSet<_>>();
    if names.iter().map(String::as_str).collect::<BTreeSet<_>>() != required || names.len() != 3 {
        return Err(format!("unsupported cross-encoder inputs: {names:?}").into());
    }
    let sequence = model.symbols.sym("sequence_length");
    for input in 0..names.len() {
        model = model.with_input_fact(input, i64::fact([1.to_dim(), sequence.to_dim()]).into())?;
    }
    let model = model.into_optimized()?.into_runnable()?;
    for pair in pairs {
        let tokens = paired_tokens(&tokenizer, pair)?;
        let inputs = names
            .iter()
            .map(|name| {
                let data = match name.as_str() {
                    "input_ids" => tokens.get_ids(),
                    "attention_mask" => tokens.get_attention_mask(),
                    "token_type_ids" => tokens.get_type_ids(),
                    _ => return Err("unsupported cross-encoder input".into()),
                };
                Tensor::from_shape(
                    &[1, tokens.len()],
                    &data.iter().map(|v| i64::from(*v)).collect::<Vec<_>>(),
                )
                .map(TValue::from)
                .map_err(Error::from)
            })
            .collect::<Result<TVec<TValue>, Error>>()?;
        let outputs = model.run(inputs)?;
        if outputs.len() != 1 {
            return Err("cross-encoder must return exactly one output".into());
        }
        let logit = classification_logit(&outputs[0])?;
        serde_json::to_writer(
            &mut writer,
            &Score {
                id: &pair.id,
                logit,
                tokens: tokens.len(),
            },
        )?;
        writeln!(writer)?;
        writer.flush()?;
    }
    Ok(())
}

fn main() -> Result<(), Error> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [model_flag, directory, pairs_flag, pairs_path] = args.as_slice() else {
        return Err("usage: score_memory_pairs --model directory --pairs pairs.json".into());
    };
    if model_flag != "--model" || pairs_flag != "--pairs" {
        return Err("usage: score_memory_pairs --model directory --pairs pairs.json".into());
    }
    let pairs: Vec<Pair> = serde_json::from_slice(&std::fs::read(pairs_path)?)?;
    score(Path::new(directory), &pairs, std::io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifier_shape_and_finite_logits_are_required_instead_of_token_pooling() {
        assert_eq!(
            classification_logit(&Tensor::from_shape(&[1, 1], &[-4.5f32]).unwrap()).unwrap(),
            -4.5
        );
        for (shape, values) in [
            (vec![1, 2, 1], vec![1f32, 2.0]),
            (vec![1, 2], vec![1.0, 2.0]),
            (vec![1, 1], vec![f32::NAN]),
            (vec![1, 1], vec![f32::INFINITY]),
        ] {
            assert!(classification_logit(&Tensor::from_shape(&shape, &values).unwrap()).is_err());
        }
    }

    #[test]
    fn duplicate_or_empty_pair_inputs_are_rejected() {
        let pair = || Pair {
            id: "one".into(),
            query: "query".into(),
            document: "document".into(),
        };
        assert!(validate_pairs(&[pair()]).is_ok());
        assert!(validate_pairs(&[pair(), pair()]).is_err());
        assert!(validate_pairs(&[Pair {
            query: " ".into(),
            ..pair()
        }])
        .is_err());
        assert!(validate_pairs(&[Pair {
            document: "".into(),
            ..pair()
        }])
        .is_err());
    }

    #[test]
    fn pair_encoding_keeps_query_and_document_in_separate_bert_segments() {
        let vocab = [
            ("[UNK]", 0),
            ("[CLS]", 1),
            ("[SEP]", 2),
            ("query", 3),
            ("document", 4),
        ]
        .map(|(k, v)| (k.to_owned(), v));
        let model = tokenizers::models::wordpiece::WordPiece::builder()
            .vocab(vocab)
            .build()
            .unwrap();
        let mut tokenizer = tokenizers::Tokenizer::new(model);
        tokenizer.with_post_processor(Some(tokenizers::processors::bert::BertProcessing::new(
            ("[SEP]".into(), 2),
            ("[CLS]".into(), 1),
        )));
        let pair = Pair {
            id: "one".into(),
            query: "query".into(),
            document: "document".into(),
        };
        let tokens = paired_tokens(&tokenizer, &pair).unwrap();
        assert_eq!(tokens.get_ids(), [1, 3, 2, 4, 2]);
        assert_eq!(tokens.get_type_ids(), [0, 0, 0, 1, 1]);
    }
}
