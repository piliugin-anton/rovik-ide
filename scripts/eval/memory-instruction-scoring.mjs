/** Native ONNX Runtime inputs for one unpadded Qwen-style instruction prompt. */
export function instructionFeeds(ort, metadata, input, cache = { length: 0, tensors: {} }) {
  const ids = input.input_ids;
  if (
    !Array.isArray(ids) ||
    !ids.length ||
    ids.length > 512 ||
    ids.some((id) => !Number.isSafeInteger(id) || id < 0)
  )
    throw new Error('Expected an unclipped instruction prompt of 1–512 token IDs');
  const required = new Set(['input_ids', 'attention_mask', 'position_ids']);
  if (!Number.isSafeInteger(cache.length) || cache.length < 0 || cache.length + ids.length > 512)
    throw new Error('Invalid cached instruction prefix length');
  const feeds = {};
  for (const item of metadata) {
    if (Object.hasOwn(feeds, item.name)) throw new Error('Duplicate model input');
    if (required.has(item.name)) {
      if (item.type !== 'int64' || item.shape.length !== 2)
        throw new Error('Expected a two-dimensional int64 prompt input');
      const values =
        item.name === 'input_ids'
          ? ids
          : item.name === 'attention_mask'
            ? Array(cache.length + ids.length).fill(1)
            : ids.map((_, i) => cache.length + i);
      feeds[item.name] = new ort.Tensor('int64', BigInt64Array.from(values.map(BigInt)), [
        1,
        values.length,
      ]);
    } else if (/^past_key_values\.\d+\.(key|value)$/.test(item.name)) {
      const [batch, heads, , width] = item.shape;
      if (
        item.type !== 'float32' ||
        item.shape.length !== 4 ||
        (typeof batch === 'number' && batch !== 1) ||
        !Number.isSafeInteger(heads) ||
        heads <= 0 ||
        !Number.isSafeInteger(width) ||
        width <= 0
      )
        throw new Error('Unsupported initial key/value cache metadata');
      if (cache.length) {
        const tensor = cache.tensors[item.name];
        if (
          !tensor ||
          tensor.type !== 'float32' ||
          tensor.dims.length !== 4 ||
          tensor.dims.some((n, i) => n !== [1, heads, cache.length, width][i])
        )
          throw new Error('Cached prefix tensor does not match model metadata');
        feeds[item.name] = tensor;
      } else {
        feeds[item.name] = new ort.Tensor('float32', new Float32Array(0), [1, heads, 0, width]);
      }
    } else {
      throw new Error(`Unsupported instruction-model input: ${item.name}`);
    }
  }
  if ([...required].some((name) => !Object.hasOwn(feeds, name)))
    throw new Error('Missing instruction-model prompt input');
  return feeds;
}

/** Read yes/no logits at the final prompt position, without generating text. */
export function instructionScore(logits, input) {
  const dims = logits.dims;
  if (
    logits.type !== 'float32' ||
    dims.length !== 3 ||
    dims[0] !== 1 ||
    dims[1] !== input.input_ids.length ||
    !Number.isSafeInteger(dims[2]) ||
    dims[2] <= 0 ||
    logits.data.length !== dims[1] * dims[2]
  )
    throw new Error('Expected full float32 vocabulary logits for one prompt');
  const labels = [input.yes_token_id, input.no_token_id];
  if (
    labels[0] === labels[1] ||
    labels.some((id) => !Number.isSafeInteger(id) || id < 0 || id >= dims[2])
  )
    throw new Error('Invalid yes/no label token IDs');
  const offset = (dims[1] - 1) * dims[2];
  const [yes, no] = labels.map((id) => logits.data[offset + id]);
  if (!Number.isFinite(yes) || !Number.isFinite(no)) throw new Error('Non-finite yes/no logits');
  const logOdds = yes - no;
  const probability =
    logOdds >= 0 ? 1 / (1 + Math.exp(-logOdds)) : Math.exp(logOdds) / (1 + Math.exp(logOdds));
  return {
    id: input.id,
    yes_logit: yes,
    no_logit: no,
    log_odds: logOdds,
    probability,
    tokens: input.input_ids.length,
  };
}
