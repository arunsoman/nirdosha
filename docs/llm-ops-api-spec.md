# LLM-Ops API Specification v1.0

A unified REST + WebSocket API that wraps the most frequently-performed,
painful LLM operations behind one consistent interface.

Design goals:
  1. One API, many backends -- TRL, Axolotl, Unsloth, vLLM, llama.cpp,
     lm-eval-harness, Outlines, AutoGPTQ, llmlingua.
  2. Async-first -- every long-running job returns a `job_id` immediately
     and streams status over WebSocket.
  3. Framework-agnostic schemas -- request/response shapes are defined by
     what the *task* needs, not what the underlying tool exposes.

Base URL:  `https://api.llm-ops.example/v1`
Content-Type: `application/json`
Auth: `Authorization: Bearer <token>`

------------------------------------------------------------------------
TABLE OF CONTENTS
------------------------------------------------------------------------
  1.  Common Types
  2.  Jobs API (shared lifecycle)
  3.  Datasets API
  4.  Models API
  5.  Training API
      5.1  SFT (Supervised Fine-Tuning)
      5.2  DPO / KTO / ORPO (Preference Alignment)
      5.3  PPO / GRPO (Reinforcement Learning)
      5.4  Reward Model Training
      5.5  Full RLHF Pipeline (SFT -> RM -> PPO)
  6.  Distillation API
      6.1  Logit Distillation
      6.2  Response Distillation (Synthetic SFT)
      6.3  Preference Distillation (Synthetic DPO)
  7.  Quantization API
  8.  Merging API
  9.  Inference & Serving API
      9.1  Deploy / Serve
      9.2  Chat Completions (OpenAI-compatible)
      9.3  Structured / Constrained Generation
      9.4  Embeddings
  10. Evaluation API
  11. Data Curation API
      11.1 Synthetic Data Generation
      11.2 Dataset Filtering & Dedup
      11.3 Preference Pair Generation
  12. Tokenization & Context Management API
  13. Error Codes
  14. Examples

------------------------------------------------------------------------
1. COMMON TYPES
------------------------------------------------------------------------

```
type JobStatus =
    "queued" | "starting" | "running" | "streaming"
  | "pausing" | "paused" | "resuming"
  | "completed" | "failed" | "cancelled"

interface JobRef {
  job_id:       string          // uuid
  type:         string          // sft | dpo | ppo | grpo | distill | quant | merge | serve | eval | ...
  status:       JobStatus
  created_at:   string          // ISO-8601
  updated_at:   string
  message:      string          // human-readable
  progress:     number          // 0.0 - 1.0
  result_url:   string?         // set on completion
  logs_url:     string?         // streaming log endpoint
  metrics_url:  string?         // streaming metrics endpoint
  error:        ErrorBody?      // set on failure
}

interface ErrorBody {
  code:       string            // see §13
  message:    string
  details:    object?
  retryable:  boolean
}

interface ModelRef {
  model_id?:    string           // hf repo id, local path, or registered model
  revision?:    string           // git sha or HF revision
  adapter_id?:  string           // LoRA/QLoRA adapter repo
  merge_adapters_on_save?: boolean // merge LoRA weights into base before saving
  quantization?: "none"|"4bit"|"8bit"|"awq"|"gptq"|"gguf_q4_k_m"
}

interface DatasetRef {
  dataset_id?:  string           // HF dataset repo
  split?:       string           // train | validation | test
  uri?:         string           // s3:// | file:// | hf://
  format:       "sft"|"preference"|"prompt_only"|"raw"
}

interface HardwareSpec {
  gpu_type?:       string        // e.g. "A100-80GB"
  num_gpus?:       integer      // for tensor_parallel
  tensor_parallel_size?: integer
  data_parallel_size?: integer
  cpu_offload?:    boolean
  gpu_memory_utilization?: number  // 0.0-1.0  (vLLM)
  mixed_precision?: "bf16"|"fp16"|"fp8"
}
```

------------------------------------------------------------------------
2. JOBS API  (shared lifecycle for all long-running operations)
------------------------------------------------------------------------

All long operations return 202 Accepted with a JobRef. Poll or subscribe.

POST   /v1/jobs/{job_id}/pause
POST   /v1/jobs/{job_id}/resume
POST   /v1/jobs/{job_id}/cancel
GET    /v1/jobs/{job_id}                       -> JobRef (latest)
GET    /v1/jobs                                -> JobRef[]  (list / filter)
WS     /v1/jobs/{job_id}/stream                -> live status frames
WS     /v1/jobs/{job_id}/logs                 -> live stdout/stderr lines
WS     /v1/jobs/{job_id}/metrics              -> live training metrics

WebSocket frame (status):
```
{ "type":"status", "job_id":"...", "status":"running",
  "progress":0.42, "step":1500, "total_steps":3500,
  "message":"epoch 1/3, loss=0.4123" }
```

WebSocket frame (metrics):
```
{ "type":"metrics", "job_id":"...", "step":1500,
  "metrics": { "train/loss":0.4123, "train/lr":2e-5,
               "eval/loss":0.4301, "eval/accuracy":0.88 } }
```

------------------------------------------------------------------------
3. DATASETS API
------------------------------------------------------------------------

POST   /v1/datasets                          -> upload / register a dataset
GET    /v1/datasets/{dataset_id}
POST   /v1/datasets/{dataset_id}/preview     -> first N rows
POST   /v1/datasets/{dataset_id}/convert     -> format conversion (sft<->preference etc.)
POST   /v1/datasets/{dataset_id}/filter       -> rule-based filtering
POST   /v1/datasets/{dataset_id}/dedup        -> MinHash / semantic dedup
POST   /v1/datasets/{dataset_id}/token-stats  -> token counts, length dist
DELETE /v1/datasets/{dataset_id}

Dataset formats:

  sft:        { "messages": [{"role":"user","content":"..."},
                            {"role":"assistant","content":"..."}] }
  preference: { "prompt":"...", "chosen":"...", "rejected":"..." }
  prompt_only:{ "prompt":"..." }
  raw:        { "text":"..." }

------------------------------------------------------------------------
4. MODELS API
------------------------------------------------------------------------

GET    /v1/models                              -> registered models
GET    /v1/models/{model_id}                   -> metadata, size, vocab
POST   /v1/models/{model_id}/clone             -> fork to local registry
POST   /v1/models/{model_id}/export            -> export to GGUF / safetensors
DELETE /v1/models/{model_id}

POST   /v1/models/{model_id}/adapters          -> list LoRA adapters
POST   /v1/models/{model_id}/merge-adapters    -> merge LoRA into base
POST   /v1/models/{model_id}/push-to-hub       -> upload to HuggingFace Hub

------------------------------------------------------------------------
5. TRAINING API
------------------------------------------------------------------------

Each training operation returns 202 + JobRef.
Common request fields:

```
interface TrainingRequest {
  base_model:        ModelRef
  output_dir:         string            // where checkpoints land
  datasets:          DatasetRef[]      // train + validation
  method:            string            // per endpoint
  hyperparameters:   object            // method-specific, see below
  hardware?:         HardwareSpec
  backend?:          "trl"|"axolotl"|"unsloth"   // auto-selected if absent
  peft?: {
    enabled:           boolean
    type?:             "lora"|"qlora"|"ia3"|"adalora"
    r?:                integer         // LoRA rank (default 16)
    alpha?:            integer         // LoRA alpha (default 32)
    dropout?:          number
    target_modules?:   string[]        // ["q_proj","v_proj", ...]
    bits?:             4|8             // QLoRA
  }
  training?: {
    epochs?:          integer
    max_steps?:       integer         // overrides epochs
    batch_size:       integer         // per-device
    gradient_accumulation_steps?: integer
    learning_rate:    number
    warmup_ratio?:    number          // default 0.03
    lr_scheduler?:   "cosine"|"linear"|"constant"|"constant_with_warmup"
    weight_decay?:    number
    max_seq_length?:  integer
    save_strategy?:  "steps"|"epoch"|"no"
    save_steps?:      integer
    eval_steps?:       integer
    logging_steps?:    integer
    seed?:             integer
    bf16?:             boolean
    gradient_checkpointing?: boolean
    packing?:          boolean        // SFT sequence packing
  }
  evaluation?: {
    eval_dataset?:    DatasetRef
    benchmarks?:      string[]       // e.g. ["mmlu","gsm8k"]
    eval_every_n_steps?: integer
  }
  wandb?: { "project":"...", "run_name":"..." }   // or tensorboard
  dry_run?:          boolean          // validate config only, don't train
}
```

========================= 5.1 SFT ============================

POST /v1/train/sft

method = "sft"
hyperparameters (sft-specific):
```
{
  "completion_only_loss": true,       // mask prompt tokens, only loss on completion
  "instruction_template": "chatml",    // chatml | alpaca | vicuna | custom
  "custom_template": "...",            // if custom
  "response_template": "\n### Response:",
  "packing": false,                    // concat sequences for efficiency
  "max_seq_length": 2048,
  "dataset_text_field": "text"        // for raw-format datasets
}
```

==================== 5.2 DPO / KTO / ORPO ===================

POST /v1/train/dpo
POST /v1/train/kto
POST /v1/train/orpo

method = "dpo" | "kto" | "orpo"
hyperparameters (dpo-family):
```
{
  "beta": 0.1,                        // KL penalty strength (DPO)
  "label_smoothing": 0.0,             // cDPO
  "loss_type": "sigmoid",             // sigmoid | hinge | ipo | kto_pair
  "max_prompt_length": 512,
  "max_length": 1024,
  "reference_free": false,            // for ORPO
  "precompute_ref_log_probs": true
}
```

============ 5.3 PPO / GRPO (Reinforcement Learning) =========

POST /v1/train/ppo
POST /v1/train/grpo

method = "ppo" | "grpo"
hyperparameters:
```
{
  "reward_model_id": "model-id",       // PPO only; GRPO uses reward_funcs
  "reward_funcs": [                   // GRPO -- mixed reward signals allowed
    { "type": "length", "params": { "min":10, "max":200 } },
    { "type": "format", "params": { "pattern": "<answer>.*</answer>" } },
    { "type": "model",  "params": { "model_id":"rm-001" } },
    { "type": "python", "params": { "code": "def reward(prompts, completions, **kw): ..." } }
  ],
  "num_generations": 4,                // GRPO: completions per prompt
  "max_new_tokens": 128,
  "kl_coef": 0.05,                     // PPO KL penalty
  "cliprange": 0.2,
  "vf_coef": 0.1,
  "total_episodes": 10000,
  "advantage_whitening": "by_std"
}
```

============== 5.4 Reward Model Training ====================

POST /v1/train/reward-model

method = "reward_model"
hyperparameters:
```
{
  "loss_type": "bradley_terry",        // bradley_terry | contrastive
  "num_labels": 1,
  "max_length": 1024
}
```

============== 5.5 Full RLHF Pipeline ======================

POST /v1/train/rlhf   (orchestrated pipeline)

```
{
  "base_model": ModelRef,
  "stages": [
    { "type": "sft",        "dataset": DatasetRef, "hyperparameters": {...} },
    { "type": "reward_model","dataset": DatasetRef, "hyperparameters": {...} },
    { "type": "ppo",        "reward_model_from_stage": 1, "hyperparameters": {...} }
  ],
  "evaluate_after_each_stage": true
}
```
Returns a single job_id; sub-jobs appear as nested JobRefs in the stream.

------------------------------------------------------------------------
6. DISTILLATION API
------------------------------------------------------------------------

The three real distillation patterns in modern LLM work.

================ 6.1 Logit / KL Distillation =================

Teacher produces soft targets (logits/probs) over the vocabulary;
student matches them via KL-divergence loss while generating the same
tokens. Most faithful, requires loading the teacher alongside.

POST /v1/distill/logits

```
{
  "teacher_model":     ModelRef,
  "student_model":     ModelRef,
  "dataset":           DatasetRef,          // prompt-only
  "output_dir":        string,
  "hyperparameters": {
    "temperature":     2.0,                 // softmax temp for softening
    "kl_weight":       1.0,
    "hard_label_weight": 0.0,               // add CE loss on true tokens
    "max_length":      1024,
    "batch_size":      4,
    "learning_rate":   5e-5,
    "epochs":          3
  },
  "hardware":          HardwareSpec,
  "backend":           "trl"                // or "custom"
}
```

============= 6.2 Response Distillation (Synthetic SFT) ========

Teacher generates completions for a prompt set; those completions
become SFT targets for the student. No teacher loaded at train time --
much cheaper. This is "distillation via synthetic data."

POST /v1/distill/responses

```
{
  "teacher_model":     ModelRef,            // or teacher_endpoint
  "teacher_endpoint":  string?,             // e.g. https://api.openai.com  (API teacher)
  "teacher_api_model": string?,             // "gpt-4o" etc. if endpoint given
  "student_model":     ModelRef,
  "prompt_dataset":    DatasetRef,          // prompt-only
  "output_sft_dataset": string,             // registered dataset id for created SFT data
  "generation": {
    "max_new_tokens":  512,
    "temperature":     0.7,
    "n_per_prompt":    1,                   // multiple samples -> SFT pool
    "batch_size":      32,
    "dedup":           true,
    "min_length":      50                   // filter degenerate outputs
  },
  "then_train_sft": {
    "enabled":          true,
    "output_dir":       string,
    "hyperparameters":  { ... }             // same as /v1/train/sft
  }
}
```
This returns a single job that runs: generate -> filter -> (optional) train.

=========== 6.3 Preference Distillation (Synthetic DPO) ======

Use a teacher (or judge model) to rank student-generated responses,
creating synthetic preference pairs for DPO/KTO/ORPO. This is the
RLAIF / constitutive-AI pattern.

POST /v1/distill/preferences

```
{
  "judge_model":       ModelRef,            // or judge_endpoint
  "judge_endpoint":    string?,
  "judge_api_model":   string?,
  "student_model":     ModelRef,            // generates candidate responses
  "prompt_dataset":    DatasetRef,
  "output_pref_dataset": string,            // registered preference dataset id
  "generation": {
    "n_per_prompt":    2,                   // >=2 candidates to rank
    "max_new_tokens":  512,
    "temperature":     0.8                  // need diversity!
  },
  "judge_mode":        "pairwise",          // pairwise | listwise | pointwise
  "judge_prompt_template": "constitutive_v1",  // or "custom"
  "ranking_method":    "bradley_terry",     // bradley_terry | elo | margin
  "filter": {
    "min_score_gap":   0.1,                 // reject near-ties
    "drop_ties":       true
  },
  "then_train_dpo": {
    "enabled":          true,
    "output_dir":       string,
    "hyperparameters":  { ... }             // same as /v1/train/dpo
  }
}
```

------------------------------------------------------------------------
7. QUANTIZATION API
------------------------------------------------------------------------

POST /v1/quantize

```
{
  "model":             ModelRef,
  "output_dir":        string,
  "method":            "gguf"|"awq"|"gptq"|"bitsandbytes"|"fp8",
  "bits":              4|8,                 // only for some methods
  "gguf_quant":        "Q4_K_M"|"Q5_K_M"|"Q6_K"|"Q8_0"|"IQ2_XXS",
  "group_size":        128,                 // AWQ/GPTQ
  "calibration_dataset": DatasetRef,        // AWQ/GPTQ need this
  "calibration_samples": 128,
  "format":            "gguf"|"safetensors"|"auto",  // output format
  "target":            "vllm"|"llamacpp"|"transformers",  // who will serve it
  "trust_remote_code": false
}
```
Returns 202 + JobRef. Result is the quantized model registered in Models API.

Also: one-shot helper
POST /v1/models/{model_id}/quantize   (shorthand, same body minus model field)

------------------------------------------------------------------------
8. MERGING API
------------------------------------------------------------------------

POST /v1/merge

Model merging is a frequent pain point -- TIES, DARE, SLERP, linear;
mergekit wraps these but the config is gnarly. This normalizes it.

```
{
  "models": [
    { "model_id": "...", "weight": 0.5, "adapter_id": "..."? },
    { "model_id": "...", "weight": 0.5 }
  ],
  "method":         "linear"|"ties"|"dare_ties"|"slerp"|"task_arithmetic",
  "slerp_t":        0.5,                  // only for slerp
  "density":        0.5,                   // DARE density
  "int8_mask":      true,                  // DARE-TIES
  "output_dir":     string,
  "output_name":    string
}
```
Returns 202 + JobRef.

------------------------------------------------------------------------
9. INFERENCE & SERVING API
------------------------------------------------------------------------

============ 9.1 Deploy / Serve ===========================

POST /v1/serve

Spin up an OpenAI-compatible endpoint running on vLLM, llama-server,
or llama-cpp-python. Long-running; status=running when ready.

```
{
  "model":         ModelRef,                // can be quantized model
  "engine":        "vllm"|"llamacpp"|"transformers",
  "port":          8000,
  "host":          "0.0.0.0",
  "max_model_len": 8192,
  "tensor_parallel_size":   1,
  "gpu_memory_utilization": 0.9,
  "enable_prefix_caching":  true,
  "enable_chunked_prefill": false,
  "quantization":  "awq"|"gptq"|"fp8"|"none",
  "speculative": {
    "draft_model":  ModelRef?,
    "num_speculative_tokens": 5
  },
  "max_num_seqs":  256,
  "trust_remote_code": false
}
```
Returns 202 + JobRef. When status=running, an `endpoint_url` is set
(e.g. http://gpu-node-3:8000/v1).

GET    /v1/serve/{job_id}                   -> includes endpoint_url when up
DELETE /v1/serve/{job_id}                   -> tear down

============ 9.2 Chat Completions  ========================

POST /v1/serve/{job_id}/chat/completions     (OpenAI-compatible)
POST /v1/serve/{job_id}/completions
GET  /v1/serve/{job_id}/models               (list served models)
GET  /v1/serve/{job_id}/metrics               (Prometheus scrape)

These are 1:1 with the OpenAI API so existing clients just point
their base_url at this endpoint.

============ 9.3 Structured / Constrained Gen ============

POST /v1/serve/{job_id}/structured

Outlines / xgrammar / llama.cpp grammar support normalized:

```
{
  "prompt":       "...",
  "messages":     [...],                    // chat form, alternative to prompt
  "constraint": {
    "type":       "json_schema"|"pydantic"|"regex"|"choice"|"grammar",
    "json_schema": {...}?,                   // type=json_schema
    "pydantic":   "ClassName"?,              // pre-registered class
    "pattern":    "..."?,                    // type=regex
    "choices":    [...]?,                    // type=choice
    "grammar":    "..."?                     // type=grammar (EBNF / GBNF)
  },
  "max_tokens":   512,
  "temperature":  0.0,                      // usually 0 for structured
  "backend":       "outlines"|"xgrammar"|"llama_grammar",
  "retry_on_parse_error": false              // false = grammar-enforced (no retries)
}
```
Response:
```
{
  "content": "<json matching schema>",
  "parsed": { ... },                        // deserialized for type=json_schema
  "usage": { "prompt_tokens": N, "completion_tokens": M }
}
```

============ 9.4 Embeddings ==============================

POST /v1/serve/{job_id}/embeddings           (OpenAI-compatible)
POST /v1/serve/{job_id}/embeddings/batch     (batched)

------------------------------------------------------------------------
10. EVALUATION API
------------------------------------------------------------------------

POST /v1/eval

Wraps lm-eval-harness (and optionally custom tasks):

```
{
  "model":           ModelRef,              // or served endpoint
  "served_endpoint": string?,               // if model is already deployed
  "tasks":           ["mmlu","gsm8k","hellaswag","truthfulqa","arc_challenge",
                      "human_eval"],        // supports list-mode names
  "custom_tasks":    [{ "name":"...", "dataset": DatasetRef,
                        "metric":"exact_match",
                        "fewshot":0 }],     // user-defined eval tasks
  "num_fewshot":     5,
  "batch_size":      "auto"|"8",
  "limit":           null,                  // restrict N samples (for speed)
  "device":          "cuda:0",
  "backend":         "hf"|"vllm",           // vllm = 5-10x faster
  "output_path":     "results/run-001",
  "log_samples":     true,                  // save individual predictions
  "confirm_unsafe_code": false              // required true for HumanEval
}
```
Returns 202 + JobRef. Result_download_url points to the JSON results blob.

GET    /v1/eval/compare?runs=run-001,run-002  -> comparison table
POST   /v1/eval/custom-task                    -> register a custom task
GET    /v1/eval/tasks                          -> list all available tasks

------------------------------------------------------------------------
11. DATA CURATION API
------------------------------------------------------------------------

The *preparation* side of training/synthesizing is often the hardest part.

============ 11.1 Synthetic Data Generation ===============

POST /v1/data/synthesize

Generate instruction-following data from a seed corpus. Useful for
cheap SFT bootstrap when you don't have labelled data.

```
{
  "generator_model":    ModelRef,           // or generator_endpoint
  "generator_endpoint": string?,
  "seed_prompts":       DatasetRef,         // or generate from scratch
  "mode":              "self_instruct"|"evol_instruct"|"magpie"|"backtranslation",
  "evol_methods":      ["simplify","elaborate","add_constraint"],  // evol_instruct
  "n_instructions":    1000,
  "max_instruction_length": 256,
  "max_response_length": 512,
  "dedup":             true,
  "dedup_method":      "minhash"|"semantic",
  "min_quality_score": 0.5,                 // judge-model filter
  "output_dataset":    string
}
```

============ 11.2 Dataset Filtering & Dedup ==============

POST /v1/data/filter
POST /v1/data/dedup
POST /v1/data/quality-score

```
POST /v1/data/filter
{
  "input":  DatasetRef,
  "output": string,
  "rules": [
    { "type":"length",         "min":50, "max":4096 },
    { "type":"language",       "langs":["en","fr"] },
    { "type":"perplexity",     "model":"...",  "max":50 },
    { "type":"keyword_blocklist","terms":["..."] },
    { "type":"pii_redact",     "entities":["email","phone","ssn"] },
    { "type":"toxicity",       "threshold":0.8 },
    { "type":"decontamination","benchmark":"mmlu", "overlap_window":8 }
  ]
}

POST /v1/data/dedup
{
  "input":   DatasetRef,
  "output":  string,
  "method":  "minhash"|"exact"|"embedding",
  "threshold":0.8,                          // jaccard for minhash
  "ngram":   5,
  "embedding_model": ModelRef?             // for embedding dedup
}

POST /v1/data/quality-score
{
  "input":   DatasetRef,
  "model":   ModelRef,                      // judge model
  "output":  string,
  "metrics": ["instruction_following","verbosity","coherence","safety","format"]
}
```

============ 11.3 Preference Pair Generation ==============

POST /v1/data/preference-pairs

Build preference data from student generations + judge, without
necessarily training immediately. Same generation/judge machinery as
/distill/preferences but stops at dataset creation.

```
{
  "student_model":     ModelRef,
  "judge_model":       ModelRef?,
  "judge_endpoint":    string?,
  "prompt_dataset":    DatasetRef,
  "n_per_prompt":      2,
  "generation":        { "max_new_tokens":512, "temperature":0.8 },
  "judge_mode":        "pairwise",
  "ranking_method":    "bradley_terry"|"elo",
  "output_dataset":    string
}
```

------------------------------------------------------------------------
12. TOKENIZATION & CONTEXT MANAGEMENT API
------------------------------------------------------------------------

POST /v1/tokenize                          -> token ids from text
POST /v1/detokenize                        -> text from ids
POST /v1/context/compress                  -> LLMLingua-style prompt compression
POST /v1/context/chunk                     -> chunk a long doc for RAG ingest
GET  /v1/models/{model_id}/tokenizer-info

```
POST /v1/context/compress
{
  "text":     "...",
  "model":    ModelRef,                    // tokenizer + LM for compression
  "rate":     0.5,                          // target compression ratio
  "method":   "llmlingua"|"llmlingua2",
  "force_tokens": ["\n","Question:","Answer:"]
}
-> { "compressed_text":"...", "original_tokens":2048, "compressed_tokens":1024,
     "ratio":0.50 }

POST /v1/context/chunk
{
  "text":       "...",
  "model":      ModelRef,
  "chunk_size": 1024,                       // tokens
  "overlap":    128,
  "strategy":   "fixed"|"semantic"|"recursive",
  "separators": ["\n\n", "\n", ". "]
}
-> { "chunks": ["...", "...", ...], "n":12 }
```

------------------------------------------------------------------------
13. ERROR CODES
------------------------------------------------------------------------

  ERR_BAD_REQUEST          400   malformed request
  ERR_UNAUTHORIZED         401   auth failed
  ERR_FORBIDDEN             403   quota / access denied
  ERR_NOT_FOUND             404   model / dataset / job not found
  ERR_CONFLICT              409   resource already in use (model locked, etc.)
  ERR_UNPROCESSABLE         422   request valid but unsupported combination
  ERR_RATE_LIMITED          429
  ERR_INTERNAL              500   server-side fault
  ERR_HARDWARE_UNAVAILABLE 503   no GPUs free
  ERR_JOB_FAILED            --    job failed (see JobRef.error)
  ERR_OOM                   --    out-of-memory during training/serving
  ERR_DATASET_INVALID       --    dataset format mismatch
  ERR_QUANTIZATION_FAILED   --    quant backend failed
  ERR_MERGE_CONFLICT        --    incompatible architectures during merge
  ERR_EVAL_TASK_UNKNOWN     --    unknown benchmark name
  ERR_CONSTRAINT_VIOLATION  --    structured-gen schema failed (shouldn't happen)

All errors include retryable flag. Clients should retry on 429, 503,
ERR_INTERNAL with retryable=true, ERR_OOM (with reduced batch_size).

------------------------------------------------------------------------
14. EXAMPLES
------------------------------------------------------------------------

--- Example A: SFT a Qwen 0.5B on Capybara, with LoRA --------------------

```
POST /v1/train/sft
{
  "base_model": { "model_id": "Qwen/Qwen2.5-0.5B" },
  "output_dir": "qwen-capybara-sft",
  "datasets": [
    { "dataset_id": "trl-lib/Capybara", "split": "train", "format": "sft" }
  ],
  "method": "sft",
  "peft": {
    "enabled": true, "type": "lora",
    "r": 16, "alpha": 32, "target_modules": ["q_proj","v_proj"]
  },
  "training": {
    "epochs": 3, "batch_size": 4, "learning_rate": 2e-5,
    "max_seq_length": 2048, "gradient_checkpointing": true, "bf16": true
  },
  "evaluation": {
    "eval_dataset": { "dataset_id":"trl-lib/Capybara", "split":"test" },
    "eval_every_n_steps": 200
  },
  "wandb": { "project": "qwen-finetune", "run_name": "capybara-sft-v1" }
}

-> 202 Accepted
{
  "job_id": "f3c1...e9a",
  "type": "sft", "status": "queued",
  "logs_url": "wss://api.../v1/jobs/f3c1...e9a/logs",
  "metrics_url": "wss://api.../v1/jobs/f3c1...e9a/metrics"
}
```

--- Example B: GRPO with a code-format reward ---------------------------

```
POST /v1/train/grpo
{
  "base_model": { "model_id": "Qwen/Qwen2.5-0.5B-Instruct" },
  "output_dir": "qwen-grpo",
  "datasets": [
    { "dataset_id": "trl-lib/tldr", "split": "train", "format": "prompt_only" }
  ],
  "method": "grpo",
  "hyperparameters": {
    "reward_funcs": [
      { "type": "length",  "params": { "min": 10, "max": 200 } },
      { "type": "python", "params": { "code":
          "def reward(prompts, completions, **kwargs):\n
             return [len(set(c.split())) / 100.0 for c in completions]" } }
    ],
    "num_generations": 4,
    "max_new_tokens": 128
  },
  "training": { "epochs": 1, "batch_size": 4, "learning_rate": 1e-5, "bf16": true }
}
```

--- Example C: Response distillation from GPT-4o, then SFT ---------------

```
POST /v1/distill/responses
{
  "teacher_endpoint":   "https://api.openai.com",
  "teacher_api_model":  "gpt-4o",
  "student_model":      { "model_id": "Qwen/Qwen2.5-1.5B" },
  "prompt_dataset":     { "dataset_id":"my-domain-prompts","format":"prompt_only" },
  "output_sft_dataset": "distilled-sft-v1",
  "generation": { "max_new_tokens": 512, "temperature": 0.7,
                  "n_per_prompt": 1, "batch_size": 32, "dedup": true },
  "then_train_sft": {
    "enabled": true,
    "output_dir": "student-distilled-v1",
    "hyperparameters": { "completion_only_loss": true,
                          "max_seq_length": 2048 }
  }
}
```

--- Example D: Quantize to GGUF for llama.cpp ---------------------------

```
POST /v1/quantize
{
  "model": { "model_id": "meta-llama/Meta-Llama-3-8B-Instruct" },
  "output_dir": "llama-3-8b-q4_k_m",
  "method": "gguf",
  "gguf_quant": "Q4_K_M",
  "format": "gguf",
  "target": "llamacpp"
}

-- shorthand --
POST /v1/models/meta-llama/Meta-Llama-3-8B-Instruct/quantize
{ "method":"gguf", "gguf_quant":"Q4_K_M" }
```

--- Example E: Preference distillation + DPO (RLAIF) -------------------

```
POST /v1/distill/preferences
{
  "judge_endpoint":      "https://api.openai.com",
  "judge_api_model":     "gpt-4o",
  "student_model":       { "model_id": "Qwen/Qwen2.5-7B-Instruct" },
  "prompt_dataset":      { "dataset_id":"my-prompts","format":"prompt_only" },
  "output_pref_dataset": "student-prefs-v1",
  "generation": { "n_per_prompt": 2, "max_new_tokens": 512, "temperature": 0.8 },
  "judge_mode":          "pairwise",
  "judge_prompt_template": "constitutive_v1",
  "filter": { "min_score_gap": 0.1, "drop_ties": true },
  "then_train_dpo": {
    "enabled": true, "output_dir": "student-dpo-v1",
    "hyperparameters": { "beta": 0.1, "max_prompt_length": 512, "max_length": 1024 }
  }
}
```

--- Example F: Serve quantized model + structured generation -------------

```
# 1. deploy
POST /v1/serve
{ "model": { "model_id":"llama-3-8b-q4_k_m" }, "engine":"llamacpp",
  "port": 8000, "max_model_len": 8192 }

# 2. structured call (Pydantic-shaped output)
POST /v1/serve/{job_id}/structured
{
  "messages": [{"role":"user","content":"Extract: Alice, 30, alice@x.com"}],
  "constraint": {
    "type": "json_schema",
    "json_schema": {
      "type":"object",
      "properties": {
        "name":  {"type":"string"},
        "age":   {"type":"integer"},
        "email": {"type":"string"}
      },
      "required":["name","age","email"]
    }
  },
  "max_tokens": 256, "temperature": 0.0,
  "backend": "outlines"
}

-> {
  "content": "{\"name\":\"Alice\",\"age\":30,\"email\":\"alice@x.com\"}",
  "parsed": { "name":"Alice","age":30,"email":"alice@x.com" },
  "usage": { "prompt_tokens": 28, "completion_tokens": 22 }
}
```

--- Example G: Evaluate + compare --------------------------------------

```
POST /v1/eval
{ "model": { "model_id":"Qwen2.5-0.5B-SFT" },
  "tasks": ["mmlu","gsm8k","hellaswag"],
  "num_fewshot": 5, "backend": "vllm",
  "output_path": "results/run-base" }

POST /v1/eval
{ "model": { "model_id":"Qwen2.5-0.5B-DPO" },
  "tasks": ["mmlu","gsm8k","hellaswag"],
  "num_fewshot": 5, "backend": "vllm",
  "output_path": "results/run-dpo" }

GET /v1/eval/compare?runs=run-base,run-dpo
-> | Model             | MMLU  | GSM8K | HELLASWAG |
   |-------------------|-------|-------|-----------|
   | Qwen2.5-0.5B-SFT  | 0.52  | 0.31  | 0.78      |
   | Qwen2.5-0.5B-DPO  | 0.53  | 0.34  | 0.79      |
```

--- Example H: Self-instruct synthetic data ----------------------------

```
POST /v1/data/synthesize
{
  "generator_model":  { "model_id":"Qwen/Qwen2.5-7B-Instruct" },
  "seed_prompts":     { "dataset_id":"seed-200","format":"prompt_only" },
  "mode":            "self_instruct",
  "n_instructions":  5000,
  "max_instruction_length":128,
  "max_response_length": 512,
  "dedup": true,
  "min_quality_score": 0.5,
  "output_dataset": "selfinstruct-5k"
}
```

========================================================================
END OF SPEC
========================================================================
