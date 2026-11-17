# Embedding model for reference search (W9)

*Status: a candidate is chosen on paper; **no model has been downloaded, run, or measured yet**. The build environment this was written in blocks `huggingface.co` at its egress proxy, so everything below about the candidates (file names, sizes, licenses, quality) is from prior knowledge of the published models and must be confirmed by the first person who runs the fetch tool on an open network. What was actually run is listed in section 4.*

## 1. What the kernel needs

ARCHITECTURE 8 and BUILD-SPEC W9: a small multilingual sentence-embedding model, permissive license, ONNX weights of at most 300 MB, run on CPU through ONNX Runtime in the kernel, bundled so that search works offline. `embed.texts` embeds reference chunks of 800 to 1200 characters (PROTOCOL 8.1); the core embeds search queries with the same model. Acceptance: 1000 chunks in under 60 s on CI CPU.

## 2. Candidates considered

Sizes are approximate and unverified (see the status line).

| model | license | languages | dims | max tokens | ONNX size | verdict |
|---|---|---|---|---|---|---|
| `intfloat/multilingual-e5-small` | MIT | about 100 | 384 | 512 | fp32 about 470 MB, fp16 about 235 MB, int8 about 120 MB | **chosen**, in the int8 or fp16 build |
| `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2` | Apache-2.0 | about 50 | 384 | 128 (training length) | same size class as e5-small | fallback: older, tuned for paraphrase rather than retrieval, short training length for our chunk size |
| `BAAI/bge-small-en-v1.5` | MIT | English only | 384 | 512 | about 130 MB | rejected: not multilingual |
| `thenlper/gte-small` | MIT | English only | 384 | 512 | about 130 MB | rejected: not multilingual |
| `Alibaba-NLP/gte-multilingual-base` | Apache-2.0 | about 70 | 768 | 8192 | over 1 GB in fp32 | rejected: over the size limit, and it needs custom model code |
| `BAAI/bge-m3` | MIT | about 100 | 1024 | 8192 | over 2 GB | rejected: far over the size limit |

Why the multilingual small models are large for their layer count: they share the XLM-R vocabulary of about 250,000 tokens, so the token-embedding table alone is about 96 million parameters (about 384 MB in fp32). That is why the fp32 export of `multilingual-e5-small` does **not** fit the 300 MB limit even though the model is "small", and why a reduced-precision build is needed.

## 3. Choice

`intfloat/multilingual-e5-small`, MIT, 384 dimensions, 512 tokens, mean pooling over the attention mask followed by L2 normalization, with the E5 input prefixes `passage: ` for chunks and `query: ` for search queries.

Open point for whoever validates it: which reduced-precision build to ship.

- **int8 (dynamic quantization), about 120 MB**: smallest and fastest on CPU; retrieval quality usually drops slightly. This is the fetch tool's default candidate (`e5-small-int8`).
- **fp16, about 235 MB**: close to fp32 quality, but ONNX Runtime's CPU provider runs fp16 slowly or casts back to fp32, so it may miss the speed criterion (`e5-small-fp16`).
- fp32 is available in the tool as `e5-small-fp32` for comparison only; the tool refuses to keep it without `--allow-large`.

The recommendation is int8 unless the evaluation in section 5 shows a visible loss on real reference sets.

## 4. What exists in the repository, and what was verified

- `python/kernel/present_kernel/refs/embed.py`: model directory resolution (`PRESENT_EMBEDDING_MODEL`, then `<home>/models/embedding/`, then `vendor/models/embedding/` in a development checkout), tokenization with `tokenizers`, truncation at the manifest's `max_tokens`, length-sorted batches of 32 padded to the longest text in the batch, inference with `onnxruntime` on CPU, mean (or CLS) pooling, L2 normalization. Prefixes, pooling, and file names come from `manifest.json` in the model directory, so no model name appears in code.
- `tools/fetch_embedding_model.py`: downloads the weights and `tokenizer.json`, enforces the 300 MB limit, writes `manifest.json` with SHA-256 checksums; `--verify` re-checks a directory. `vendor/models/` is git-ignored.
- With no model directory, `kernel.hello` reports `{"kind": "none", "model": "", "dims": 0}` and `embed.texts` fails with error 2000 and a message that says how to install a model. Lexical search does not depend on it.

Verified by tests (`python/kernel/tests/test_refs_embed_analyst.py`):

- The full path through real `onnxruntime` and `tokenizers` against a tiny ONNX model the test helpers write by hand (a single `Gather` over a random embedding matrix): vectors equal the hand-computed normalized mean of the token embeddings, padding does not leak into the mean, truncation and both prefixes apply, input order is restored after length sorting.
- Batching and pooling against a stub session (batch shapes, CLS pooling, already-pooled outputs).
- Resolution order and the no-model behavior.

**Not verified:**

- The fetch tool has never completed a download. Running it here fails at the proxy (`Tunnel connection failed: 403 Forbidden`) and exits with code 2 without writing anything. The upstream file paths in its candidate table (`onnx/model_quantized.onnx`, `onnx/model_fp16.onnx` in `Xenova/multilingual-e5-small`; `onnx/model.onnx` in `intfloat/multilingual-e5-small`) are from memory, the revisions are `main` rather than pinned commits, and the checksum pins are empty.
- The real model has never been loaded by `embed.py`. In particular: that its ONNX inputs are `input_ids` and `attention_mask` (the code also feeds `token_type_ids` when the model declares it), that its first output is the token states, and that its `tokenizer.json` adds the `<s>` and `</s>` special tokens on its own.
- Speed. The 1000-chunk test runs the tiny model and measures pipeline overhead only. The acceptance criterion (1000 chunks in under 60 s on CI CPU) is unmeasured for the real model.
- Retrieval quality, in any language.

## 5. To finish this package

1. On an open network: `python tools/fetch_embedding_model.py`, then pin the printed checksums and the upstream commit in the tool's candidate table. Confirm the license file in the upstream repository and add the model to `THIRD-PARTY.md`.
2. Start a kernel and check `kernel.hello` reports `bundled`, 384 dimensions; embed a few passages and queries in two languages and check that a query ranks its own passage first.
3. Time 1000 chunks of about 1000 characters on a CI runner. If int8 is too slow, lower `max_tokens` in the manifest to 384 (chunks rarely need more) before considering a different model.
4. Compare int8 and fp16 on the hybrid-search fixture queries of W10 (expected chunk in the top 3) and record the result here.
5. Decide how CI gets the model (a cached download step) so the timing test can run there; the unit tests do not need it.

## 6. Protocol note

PROTOCOL 1.3 defines `embed.texts { texts }` only. E5 models need different prefixes for passages and queries, so the kernel accepts an optional `kind: "passage" | "query" | "none"` (default `passage`) and also returns `model` and `dims` next to `vectors`. The core must pass `kind: "query"` when it embeds a search query. PROTOCOL.md has not been changed for this.
