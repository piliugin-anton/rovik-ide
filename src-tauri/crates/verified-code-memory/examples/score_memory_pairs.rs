//! Evaluation-only native pair scoring and instruction-model tokenization.
//! Model provisioning, causal-model inference and policy selection stay external.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, io::Write, path::Path};
use tract_onnx::prelude::*;
use verified_code_memory::MemoryEmbedder;

type Error = Box<dyn std::error::Error>;
const MAX_TOKENS: usize = 512;
const USAGE: &str = "usage: score_memory_pairs --model directory --pairs pairs.json OR --tokenize-instruction-pairs tokenizer_directory pairs.json instruction.txt (use --tokenize-bounded-instruction-pairs to report unsupported prompts) OR --embedding-reference directory texts.json cls|masked_mean";
const INSTRUCTION_PREFIX: &str = "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n";
const INSTRUCTION_SUFFIX: &str = "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n";

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

fn join_prompt_tokens(prefix: &[u32], body: &[u32], suffix: &[u32]) -> Result<Vec<u32>, Error> {
    if prefix
        .len()
        .saturating_add(body.len())
        .saturating_add(suffix.len())
        > MAX_TOKENS
    {
        return Err(
            "instruction pair exceeds the 512-token research limit; refusing to clip its prompt"
                .into(),
        );
    }
    Ok([prefix, body, suffix].concat())
}

fn instruction_body(pair: &Pair, instruction: &str) -> String {
    format!(
        "<Instruct>: {instruction}\n<Query>: {}\n<Document>: {}",
        pair.query, pair.document
    )
}

fn instruction_record(
    id: &str,
    parts: [&[u32]; 3],
    labels: (u32, u32),
    bounded: bool,
) -> Result<serde_json::Value, Error> {
    let length = parts.iter().map(|part| part.len()).sum::<usize>();
    if bounded && length > MAX_TOKENS {
        return Ok(
            serde_json::json!({"id":id,"unsupported":"prompt exceeds the research token limit","tokens":length}),
        );
    }
    let input_ids = join_prompt_tokens(parts[0], parts[1], parts[2])?;
    Ok(
        serde_json::json!({"id":id,"input_ids":input_ids,"yes_token_id":labels.0,"no_token_id":labels.1}),
    )
}

fn tokenize_instruction_pairs(
    directory: &Path,
    pairs: &[Pair],
    instruction: &str,
    bounded: bool,
    mut writer: impl Write,
) -> Result<(), Error> {
    validate_pairs(pairs)?;
    if instruction.trim().is_empty() {
        return Err("relevance instruction must be nonempty".into());
    }
    let mut tokenizer = tokenizers::Tokenizer::from_file(directory.join("tokenizer.json"))
        .map_err(|e| format!("cannot load instruction tokenizer: {e}"))?;
    tokenizer
        .with_truncation(None)
        .map_err(|e| format!("cannot disable instruction truncation: {e}"))?;
    tokenizer.with_padding(None);
    let encode = |text: &str| {
        tokenizer
            .encode(text, false)
            .map_err(|e| -> Error { format!("cannot tokenize instruction input: {e}").into() })
    };
    let prefix = encode(INSTRUCTION_PREFIX)?;
    let suffix = encode(INSTRUCTION_SUFFIX)?;
    let yes = encode("yes")?;
    let no = encode("no")?;
    if yes.len() != 1 || no.len() != 1 || yes.get_ids() == no.get_ids() {
        return Err("instruction scoring requires distinct single-token yes/no labels".into());
    }
    for pair in pairs {
        let body = encode(&instruction_body(pair, instruction))?;
        let record = instruction_record(
            &pair.id,
            [prefix.get_ids(), body.get_ids(), suffix.get_ids()],
            (yes.get_ids()[0], no.get_ids()[0]),
            bounded,
        )?;
        serde_json::to_writer(&mut writer, &record)?;
        writeln!(writer)?;
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
    if let [mode, directory, texts_path, pooling] = args.as_slice() {
        if mode == "--embedding-reference" {
            use verified_code_memory::embedding::{OnnxMiniLm, OnnxPooling};
            let texts: Vec<String> = serde_json::from_slice(&std::fs::read(texts_path)?)?;
            // Keep references below either provider window so the independent
            // engine can compare identical, untruncated token inputs.
            if texts.iter().any(|s| s.chars().count() > 200) {
                return Err("embedding references must contain at most 200 characters".into());
            }
            let pooling = match pooling.as_str() {
                "cls" => OnnxPooling::Cls,
                "masked_mean" => OnnxPooling::MaskedMean,
                _ => return Err("reference pooling must be cls or masked_mean".into()),
            };
            let encoder = OnnxMiniLm::new_with_pooling(Path::new(directory), pooling)?;
            let mut tokenizer =
                tokenizers::Tokenizer::from_file(Path::new(directory).join("tokenizer.json"))
                    .map_err(|e| format!("cannot load reference tokenizer: {e}"))?;
            tokenizer
                .with_truncation(None)
                .map_err(|e| format!("cannot disable reference truncation: {e}"))?;
            tokenizer.with_padding(None);
            let mut writer = std::io::stdout().lock();
            for text in texts {
                let tokens = tokenizer
                    .encode(text.as_str(), true)
                    .map_err(|e| format!("cannot tokenize reference: {e}"))?;
                if tokens.len() > 256 {
                    return Err("embedding reference exceeds the native 256-token window".into());
                }
                let vector = encoder.embed(&text)?;
                serde_json::to_writer(
                    &mut writer,
                    &serde_json::json!({"text":text,"vector":vector,"input_ids":tokens.get_ids(),"attention_mask":tokens.get_attention_mask(),"token_type_ids":tokens.get_type_ids()}),
                )?;
                writeln!(writer)?;
            }
            return Ok(());
        }
    }
    if let [mode, directory, pairs_path, instruction_path] = args.as_slice() {
        if mode == "--tokenize-instruction-pairs" || mode == "--tokenize-bounded-instruction-pairs"
        {
            let pairs: Vec<Pair> = serde_json::from_slice(&std::fs::read(pairs_path)?)?;
            let instruction = std::fs::read_to_string(instruction_path)?;
            return tokenize_instruction_pairs(
                Path::new(directory),
                &pairs,
                instruction.trim_end(),
                mode == "--tokenize-bounded-instruction-pairs",
                std::io::stdout().lock(),
            );
        }
    }
    let [model_flag, directory, pairs_flag, pairs_path] = args.as_slice() else {
        return Err(USAGE.into());
    };
    if model_flag != "--model" || pairs_flag != "--pairs" {
        return Err(USAGE.into());
    }
    let pairs: Vec<Pair> = serde_json::from_slice(&std::fs::read(pairs_path)?)?;
    score(Path::new(directory), &pairs, std::io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_exports_mark_unsupported_pairs_without_emitting_a_clipped_prompt() {
        let body = vec![3; 512];
        let parts: [&[u32]; 3] = [&[1], &body, &[2]];
        assert!(instruction_record("long", parts, (4, 5), false).is_err());
        let unsupported = instruction_record("long", parts, (4, 5), true).unwrap();
        assert_eq!(unsupported["tokens"], 514);
        assert!(unsupported.get("input_ids").is_none());
        assert!(unsupported["unsupported"].is_string());
        assert_eq!(
            instruction_record("short", [&[1], &[3], &[2]], (4, 5), true).unwrap()["input_ids"],
            serde_json::json!([1, 3, 2])
        );
    }

    #[test]
    fn instruction_prompt_segments_keep_source_text_and_reject_silent_clipping() {
        let pair = Pair {
            id: "one".into(),
            query: "task\ndiagnostic".into(),
            document: "title\nsource assertion".into(),
        };
        assert_eq!(instruction_body(&pair, "Judge relevance."), "<Instruct>: Judge relevance.\n<Query>: task\ndiagnostic\n<Document>: title\nsource assertion");
        assert!(INSTRUCTION_PREFIX.ends_with("<|im_start|>user\n"));
        assert_eq!(
            INSTRUCTION_SUFFIX,
            "<|im_end|>\n<|im_start|>assistant\n<think>\n\n</think>\n\n"
        );
        assert_eq!(
            join_prompt_tokens(&[1, 2], &[3, 4], &[5, 6]).unwrap(),
            [1, 2, 3, 4, 5, 6]
        );
        assert_eq!(
            join_prompt_tokens(&[1], &vec![2; 510], &[3]).unwrap().len(),
            512
        );
        assert!(join_prompt_tokens(&[1], &vec![2; 511], &[3]).is_err());
    }

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
