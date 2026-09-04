# LLM-Ops API Specification v2.0

A unified REST + WebSocket API for the most frequently-performed,
painful LLM operations.

CHANGES FROM v1 (informed by self-critique):
  - No fake backend abstraction. Each training endpoint exposes
    backend-specific hyperparameter blocks. The API does NOT pretend
    TRL, Axolotl, and Unsloth are interchangeable.
  - Judge calibration is a first-class workflow. Judge prompt templates
    must be validated before use in distillation/RLAIF.
  - Eval-during-training with auto-rollback to best checkpoint.
  - Reward functions are registered + sandboxed, not inline Python strings.
  - Real error codes for the failures people actually hit.
  - Cluster scheduling layer: GPU allocation, preemption, queueing.
  - Dataset content hashing + revision for reproducibility.
  - Monitoring: GPU utilization, gradient norms, loss spike detection,
    ETA, memory fragmentation -- not just loss curves.
  - Multi-tenant: workspaces, cost estimation, approval workflow,
    audit trail.
  - Structured generation reports coercion_notes when the model
    was forced against its preferences.

Base URL:  `https://api.llm-ops.example/v1`
Content-Type: `application/json`
Auth: `Authorization: Bearer <token>`

------------------------------------------------------------------------
TABLE OF CONTENTS
------------------------------------------------------------------------
  1.  Common Types
  2.  Multi-Tenancy & Auth
  3.  Cluster & Scheduling
  4.  Jobs API (shared lifecycle)
  5.  Monitoring API
  6.  Datasets API
  7.  Models API
  8.  Reward Functions API
  9.  Judge API
  10. Training API
      10.1 SFT
      10.2 DPO / KTO / ORPO
      10.3 PPO / GRPO
      10.4 Reward Model Training
      10.5 Full RLHF Pipeline
  11. Distillation API
      11.1 Logit Distillation
      11.2 Response Distillation (Synthetic SFT)
      11.3 Preference Distillation (Synthetic DPO / RLAIF)
  12. Quantization API
  13. Merging API
  14. Inference & Serving API
      14.1 Deploy / Serve
      14.2 Chat Completions (OpenAI-compatible)
      14.3 Structured / Constrained Generation
      14.4 Embeddings
  15. Evaluation API
  16. Data Curation API
      16.1 Synthetic Data Generation
      16.2 Dataset Filtering & Dedup
      16.3 Preference Pair Generation
  17. Tokenization & Context Management API
  18. Error Codes
  19. Examples

------------------------------------------------------------------------
1. COMMON TYPES
------------------------------------------------------------------------

```
type JobStatus =
    "queued" | "starting" | "running" | "streaming"
  | "pausing" | "paused" | "resuming"
  | "completed" | "failed" | "cancelled"

interface JobRef {
  job_id:         string           // uuid
  type:           string           // sft | dpo | ppo | grpo | distill |
                                   // quant | merge | serve | eval | ...
  status:         JobStatus
  workspace_id:   string
  created_at:     string           // ISO-8601
  updated_at:     string
  message:        string           // human-readable
  progress:       number           // 0.0 - 1.0
  step:           integer?         // current training step
  total_steps:    integer?
  result_url:     string?          // set on completion
  logs_url:       string?          // WS endpoint for stdout/stderr
  metrics_url:    string?          // WS endpoint for live metrics
  endpoint_url:   string?          // set for serve jobs when ready
  cost_estimate:  CostEstimate?
  cost_actual:    CostActual?
  error:          ErrorBody?       // set on failure
  parent_job_id:  string?          // for pipeline sub-jobs
  audit:          AuditEntry
}

interface ErrorBody {
  code:       string               // see §18
  message:    string
  details:    object?
  retryable:  boolean
  recovery_hint: string?           // suggested action for the user
}

interface ModelRef {
  model_id?:      string           // hf repo, local path, registered model
  revision?:      string           // git sha or HF revision (REQUIRED for
                                   // reproducibility; defaults to "main"
                                   // but emits a warning)
  adapter_id?:    string           // LoRA/QLoRA adapter repo
  adapter_revision?: string
  merge_adapters_on_save?: boolean
  quantization?: "none"|"4bit"|"8bit"|"awq"|"gptq"|"gguf_q4_k_m"

  // The API validates that the model's chat template is compatible
  // with the dataset format before training starts.  If there is a
  // mismatch, the job fails with ERR_CHAT_TEMPLATE_MISMATCH before
  // any GPU is allocated -- not 2 hours into a run.
  expected_chat_template?: "chatml"|"llama3"|"alpaca"|"vicuna"|
                            "mistral"|"gemma"|"custom"
  custom_chat_template?:   string  // jinja template string
}

interface DatasetRef {
  dataset_id?:    string           // registered dataset
  revision?:      string           // dataset version / git sha
  content_hash?:  string           // sha256 of row contents (strongest
                                   // reproducibility guarantee; computed
                                   // server-side on registration)
  split?:         string           // train | validation | test
  uri?:           string           // s3:// | file:// | hf://
  format:         "sft"|"preference"|"prompt_only"|"raw"
  rows?:          integer          // server fills this in
  bytes?:         integer          // server fills this in
}

interface HardwareSpec {
  gpu_type?:         string        // "A100-80GB" | "H100" | "L4" ...
  num_gpus?:         integer
  tensor_parallel_size?: integer
  data_parallel_size?: integer
  cpu_offload?:      boolean
  gpu_memory_utilization?: number  // 0.0-1.0
  mixed_precision?:  "bf16"|"fp16"|"fp8"
  max_memory_per_gpu?: string      // e.g. "70GB" for multi-tenant isolation
}

interface CostEstimate {
  estimated_gpu_hours:  number
  estimated_cost_usd:   number
  currency:             string     // "USD"
  rate_card_id:         string     // which pricing snapshot was used
  confidence:           "low"|"medium"|"high"
  // low = first-time config, no historical data
  // high = identical job ran before
}

interface CostActual {
  gpu_hours:     number
  cost_usd:      number
  started_at:    string
  ended_at:      string?
}

interface AuditEntry {
  initiated_by:  string           // user_id
  workspace_id:  string
  reason:        string?          // free-text justification
  approval_id:   string?          // if approval was required
  ip_address:    string
  user_agent:    string
}

interface ApprovalRequest {
  approval_id:    string
  job_type:       string
  requested_by:   string
  workspace_id:   string
  cost_estimate:  CostEstimate
  status:         "pending"|"approved"|"denied"|"expired"
  approver_id:    string?
  decided_at:     string?
  auto_approve_rules?: object     // see §2
}
```

------------------------------------------------------------------------
2. MULTI-TENANCY & AUTH
------------------------------------------------------------------------

Every resource belongs to a workspace.  Users belong to one or more
workspaces with roles.

```
interface Workspace {
  workspace_id:   string
  name:           string
  gpu_quota:      integer          // max concurrent GPUs
  monthly_budget_usd?: number
  approval_rules?: {
    training_cost_threshold_usd?:  number  // jobs above this need approval
    serve_cost_threshold_usd?:     number
    publish_to_hub_requires_approval: boolean
    allowed_model_licenses?:       string[]  // e.g. ["apache-2.0","mit"]
    blocked_models?:               string[]  // e.g. gated models
  }
}
```

POST   /v1/workspaces                        -> create workspace
GET    /v1/workspaces                        -> list my workspaces
GET    /v1/workspaces/{workspace_id}
PATCH  /v1/workspaces/{workspace_id}         -> update quotas / rules
DELETE /v1/workspaces/{workspace_id}

POST   /v1/workspaces/{workspace_id}/members  -> add user with role
GET    /v1/workspaces/{workspace_id}/members
DELETE /v1/workspaces/{workspace_id}/members/{user_id}

Roles: "owner" | "admin" | "developer" | "viewer"

GET    /v1/workspaces/{workspace_id}/audit-log  -> AuditEntry[]
  // every job start, approval, model publish, dataset mutation

POST   /v1/approvals/{approval_id}/approve
POST   /v1/approvals/{approval_id}/deny
GET    /v1/approvals?status=pending&workspace_id=...

When a job is submitted and the cost estimate exceeds the workspace's
training_cost_threshold_usd, the job enters "queued" status with
approval_required=true.  No GPUs are allocated until approved.

------------------------------------------------------------------------
3. CLUSTER & SCHEDULING
------------------------------------------------------------------------

The cluster layer manages GPU allocation across all jobs from all
workspaces.

```
interface ClusterNode {
  node_id:        string
  gpu_type:       string
  num_gpus:       integer
  gpus_in_use:    integer
  gpu_status:     [GpuStatus]      // per-GPU state
  memory_total_gb:  number
  memory_used_gb:   number
}

interface GpuStatus {
  gpu_index:     integer
  state:         "idle"|"busy"|"reserved"|"error"
  assigned_job_id?: string
  assigned_workspace_id?: string
  utilization_pct:  number
  memory_used_mb:   number
  memory_total_mb:  number
  temperature_c:    number
}

interface SchedulingPolicy {
  policy:        "fifo"|"priority"|"fair_share"|"preemptive_priority"
  workspace_weights?: object   // { "ws-a": 2, "ws-b": 1 }
  preemption_enabled: boolean
  preempt_serving_for_training: boolean  // can training kick off serve?
  max_concurrent_jobs_per_workspace: integer
  backfill_enabled: boolean   // fill gaps with small queued jobs
}
```

GET    /v1/cluster/nodes               -> ClusterNode[]
GET    /v1/cluster/gpus                -> all GPUs with status
GET    /v1/cluster/queue               -> queued jobs in priority order
GET    /v1/cluster/policy              -> current SchedulingPolicy
PATCH  /v1/cluster/policy              -> admin only

When a job is submitted:
  1. Cost estimate is computed (historical data or heuristic).
  2. If approval required, job enters "queued" with approval_required.
  3. If approved (or below threshold), job enters scheduling queue.
  4. Scheduler assigns GPUs based on policy.  If insufficient GPUs:
     - fifo: wait in queue
     - preemptive: may preempt lower-priority serve jobs
       (training never preempts other training -- too expensive to lose)
  5. On GPU assignment, job enters "starting" -> "running".

Memory isolation: each job gets a GPU memory limit (max_memory_per_gpu
from HardwareSpec, or workspace default).  vLLM serves within that
limit via gpu_memory_utilization.  Training jobs use PyTorch memory
fragmentation tracking -- if fragmentation exceeds 30%, an alert fires
before OOM.

------------------------------------------------------------------------
4. JOBS API
------------------------------------------------------------------------

All long operations return 202 Accepted with a JobRef.

POST   /v1/jobs/{job_id}/pause
POST   /v1/jobs/{job_id}/resume
POST   /v1/jobs/{job_id}/cancel
GET    /v1/jobs/{job_id}                   -> JobRef (latest)
GET    /v1/jobs?workspace_id=...&status=...&type=...
DELETE /v1/jobs/{job_id}                   -> cancel + cleanup

WS     /v1/jobs/{job_id}/stream            -> live status frames
WS     /v1/jobs/{job_id}/logs             -> live stdout/stderr lines
WS     /v1/jobs/{job_id}/metrics          -> live metrics (see §5)

Status frame:
```
{ "type":"status", "job_id":"...", "status":"running",
  "progress":0.42, "step":1500, "total_steps":3500,
  "message":"epoch 1/3, loss=0.4123",
  "eta_seconds": 7200,
  "cost_actual": { "gpu_hours": 1.2, "cost_usd": 4.80 }
}
```

------------------------------------------------------------------------
5. MONITORING API
------------------------------------------------------------------------

Training is not just loss curves.  The monitoring stream carries
hardware + training signals so the user never needs to SSH in.

WS /v1/jobs/{job_id}/metrics

Frame types:

```
// Training metrics (every logging_steps)
{ "type":"metrics", "job_id":"...", "step":1500,
  "metrics": {
    "train/loss": 0.4123,
    "train/learning_rate": 2e-5,
    "train/grad_norm": 0.82,
    "eval/loss": 0.4301,
    "eval/accuracy": 0.88
  }
}

// Hardware metrics (every 10s)
{ "type":"hardware", "job_id":"...",
  "gpus": [
    { "index":0, "utilization_pct":87, "memory_used_mb":38400,
      "memory_total_mb":81920, "temperature_c":71,
      "power_draw_w":350, "memory_fragmentation_pct":12 },
    { "index":1, "utilization_pct":85, "memory_used_mb":38000,
      "memory_total_mb":81920, "temperature_c":69,
      "power_draw_w":340, "memory_fragmentation_pct":8 }
  ],
  "disk": { "used_gb": 120, "total_gb": 500,
            "checkpoint_dir_gb": 45 }
}

// Alert frame (asynchronous, important)
{ "type":"alert", "job_id":"...", "severity":"warning"|"critical",
  "alert_code": "GRAD_NORM_EXPLODING",
  "message": "Gradient norm 12.4 > threshold 5.0 at step 1503",
  "auto_action_taken": "gradient_clipping_applied",
  "recommended_action": "reduce learning_rate or check dataset quality"
}

// Eval-during-training checkpoint result
{ "type":"eval_checkpoint", "job_id":"...", "step":1500,
  "checkpoint_id": "ckpt-1500",
  "benchmarks": { "gsm8k": {"exact_match":0.41},
                  "hellaswag": {"acc_norm":0.79} },
  "is_best_so_far": true,
  "auto_rollback_target": false
}

// ETA update
{ "type":"eta", "job_id":"...", "step":1500, "total_steps":3500,
  "eta_seconds": 7200,
  "tokens_per_second": 4500,
  "samples_per_second": 12.5 }
```

Alert codes (auto-detected, not user-configurable):

  GRAD_NORM_EXPLODING    grad_norm > 5.0 (or 10x running average)
                         auto: gradient clipping applied
                         action: reduce LR, check data

  LOSS_NAN               loss is NaN or Inf
                         auto: job paused, checkpoint rolled back
                         action: reduce LR, check for bad data rows

  LOSS_SPIKE             loss jumped > 3x in one step
                         auto: logged, no action
                         action: monitor; if repeats, reduce LR

  LR_WARMUP_EXPLODED     loss > 10x initial in first 50 steps
                         auto: job paused
                         action: reduce LR or warmup_ratio

  MEMORY_FRAGMENTATION   fragmentation > 30%
                         auto: warning logged
                         action: reduce batch_size or enable
                                 gradient_checkpointing

  GPU_THERMAL_THROTTLE   temperature > 85C
                         auto: logged
                         action: check cooling, reduce batch_size

  OOM_IMMINENT           memory_used > 95% of allocation
                         auto: job paused before hard OOM
                         action: reduce batch_size or max_seq_length

  EVAL_DEGRADATION       eval metric dropped > 10% from best
                         auto: rollback to best checkpoint flagged
                         action: consider early stopping

  CHECKPOINT_SAVE_FAILED shard write failed
                         auto: retry once, then alert
                         action: check disk space

  DATASET_DETOKENIZED    token-length distribution looks like
                         double-tokenization (median token count
                         2x expected)
                         auto: warning logged at job start
                         action: check dataset preprocessing

  EVAL_LEAKAGE_DETECTED  train/eval prompt overlap > 1%
                         auto: warning at eval time
                         action: re-split dataset

GET  /v1/jobs/{job_id}/alerts              -> AlertFrame[] (historical)
GET  /v1/jobs/{job_id}/hardware-history    -> hardware frames (time series)

------------------------------------------------------------------------
6. DATASETS API
------------------------------------------------------------------------

POST   /v1/datasets                       -> upload / register
GET    /v1/datasets?workspace_id=...
GET    /v1/datasets/{dataset_id}
POST   /v1/datasets/{dataset_id}/preview   -> first N rows
POST   /v1/datasets/{dataset_id}/convert    -> format conversion
POST   /v1/datasets/{dataset_id}/filter     -> rule-based filtering
POST   /v1/datasets/{dataset_id}/dedup      -> MinHash / semantic dedup
POST   /v1/datasets/{dataset_id}/token-stats-> token counts, length dist
POST   /v1/datasets/{dataset_id}/validate   -> schema + format check
DELETE /v1/datasets/{dataset_id}

On registration, the server computes:
  - content_hash (sha256 of serialized rows) -- stored permanently
  - row_count, byte_size
  - token-length distribution (requires a tokenizer_id)
  - format validation (does it actually parse as sft/preference/etc.?)
  - duplicate detection preview (% near-duplicate rows)

Every DatasetRef returned by the API includes content_hash.  Jobs
store the content_hash in their audit trail so a model can always be
traced back to the exact data it trained on.

POST /v1/datasets  (register/upload)
```
{
  "workspace_id": "...",
  "name": "my-sft-data-v3",
  "format": "sft",
  "source_uri": "s3://my-bucket/sft-v3.jsonl",  // or
  "rows_inline": [...],                          // or
  "hf_dataset_id": "trl-lib/Capybara",
  "hf_split": "train",
  "tokenizer_id": "Qwen/Qwen2.5-0.5B",          // for token stats
  "description": "Third iteration, added 500 code examples"
}
```
-> 201 Created
```
{
  "dataset_id": "ds-abc123",
  "content_hash": "sha256:9f4a...",
  "revision": "v3",
  "rows": 12500,
  "bytes": 48000000,
  "token_stats": {
    "median": 420, "p95": 1800, "max": 4096,
    "mean": 510
  },
  "duplicate_preview": { "pct_near_dup": 2.1 },
  "format_valid": true
}
```

POST /v1/datasets/{dataset_id}/validate
```
{
  "expected_format": "sft",
  "tokenizer_id": "Qwen/Qwen2.5-0.5B",
  "checks": [
    "schema",           // field names + types correct
    "token_stats",      // reasonable length distribution
    "duplicate_scan",   // near-duplicate detection
    "detokenization",   // detect double-tokenized data
    "pii_scan",         // detect PII (email, phone, SSN)
    "language_detect",  // language distribution
    "benchmark_overlap" // check overlap with eval benchmarks
  ],
  "benchmark_names": ["mmlu","gsm8k"]  // for overlap check
}
```
-> 200 OK
```
{
  "valid": false,
  "issues": [
    { "check":"detokenization", "severity":"critical",
      "message":"Median token count 920 is 2.1x expected 440. Data may be double-tokenized.",
      "affected_rows_pct": 38.2 },
    { "check":"benchmark_overlap", "severity":"warning",
      "message":"12 prompts (0.1%) overlap with gsm8k test set.",
      "affected_rows": 12 },
    { "check":"pii_scan", "severity":"warning",
      "message":"8 rows contain email addresses.",
      "affected_rows": 8 }
  ]
}
```

Dataset formats (unchanged from v1):

  sft:        { "messages": [{"role":"user","content":"..."},
                            {"role":"assistant","content":"..."}] }
  preference: { "prompt":"...", "chosen":"...", "rejected":"..." }
  prompt_only:{ "prompt":"..." }
  raw:        { "text":"..." }

------------------------------------------------------------------------
7. MODELS API
------------------------------------------------------------------------

GET    /v1/models?workspace_id=...
GET    /v1/models/{model_id}               -> metadata, size, vocab,
                                               chat_template, license
POST   /v1/models/{model_id}/clone          -> fork to workspace registry
POST   /v1/models/{model_id}/export         -> export GGUF / safetensors
DELETE /v1/models/{model_id}

POST   /v1/models/{model_id}/adapters       -> list LoRA adapters
POST   /v1/models/{model_id}/merge-adapters -> merge LoRA into base
POST   /v1/models/{model_id}/push-to-hub    -> upload to HuggingFace Hub
  // requires approval if workspace rules say so

GET    /v1/models/{model_id}/chat-template  -> returns the jinja template
POST   /v1/models/{model_id}/validate-template
  // checks template against a dataset format

------------------------------------------------------------------------
8. REWARD FUNCTIONS API
------------------------------------------------------------------------

Reward functions for GRPO/PPO are REGISTERED, not inline.  This
prevents arbitrary code execution on the GPU cluster.

POST   /v1/reward-functions              -> register a new reward function
GET    /v1/reward-functions              -> list registered functions
GET    /v1/reward-functions/{func_id}
DELETE /v1/reward-functions/{func_id}
POST   /v1/reward-functions/{func_id}/test  -> dry-run against samples

Registration options (one of):

  BUILTIN -- declarative, no code execution:
```
{
  "name": "length-reward",
  "type": "builtin",
  "builtin": {
    "kind": "length",
    "min_tokens": 10,
    "max_tokens": 200,
    "penalty_curve": "linear"     // linear | quadratic | step
  }
}
```

```
{
  "name": "format-reward",
  "type": "builtin",
  "builtin": {
    "kind": "format",
    "pattern": "<answer>.*</answer>",
    "reward_on_match": 1.0,
    "reward_on_miss": 0.0
  }
}
```

  MODEL -- use a reward model endpoint:
```
{
  "name": "my-rm",
  "type": "model",
  "model": { "model_id": "rm-001" }
}
```

  SANDBOXED_PYTHON -- restricted Python, not arbitrary:
```
{
  "name": "unique-words",
  "type": "sandboxed_python",
  "code": "def reward(prompts, completions, **kwargs):\n
              return [len(set(c.split())) / 100.0 for c in completions]",
  "allowed_modules": [],          // NO imports allowed by default
  "timeout_ms": 5000,             // hard timeout per batch
  "max_memory_mb": 256,           // memory limit
  "allowed_builtins": ["len","set","str","int","float","list",
                        "dict","range","sum","min","max","sorted",
                        "enumerate","zip","any","all"]
}
```
The sandbox:
  - Runs in a separate restricted process (not the training process).
  - No filesystem access.  No network.  No imports.
  - Hard timeout + memory limit.  Killed if exceeded.
  - Only builtins listed in allowed_builtins are available.
  - Code is validated at registration time (AST analysis rejects
    import statements, exec/eval, open(), subprocess, os.*, etc.)

  COMPOSITE -- combine multiple reward functions:
```
{
  "name": "code-quality",
  "type": "composite",
  "components": [
    { "func_id": "length-reward", "weight": 0.3 },
    { "func_id": "format-reward", "weight": 0.7 }
  ],
  "aggregation": "weighted_sum"   // weighted_sum | max | min | mean
}
```

POST /v1/reward-functions/{func_id}/test
```
{
  "prompts": ["What is 2+2?"],
  "completions": ["The answer is 4."],
  "expected_rewards": [0.8]       // optional, for validation
}
```
-> 200 OK
```
{
  "rewards": [0.72],
  "latency_ms": 12,
  "errors": []
}
```

------------------------------------------------------------------------
9. JUDGE API
------------------------------------------------------------------------

Judge models evaluate outputs for preference distillation / RLAIF.
The judge PROMPT is the single most important variable.  This API
treats judge templates as first-class, validated artifacts.

POST   /v1/judge-templates                -> register a judge template
GET    /v1/judge-templates
GET    /v1/judge-templates/{template_id}
DELETE /v1/judge-templates/{template_id}
POST   /v1/judge-templates/{template_id}/calibrate  -> validate quality
GET    /v1/judge-templates/{template_id}/calibration-report

POST /v1/judge-templates  (register)
```
{
  "workspace_id": "...",
  "name": "constitutive-v1",
  "mode": "pairwise"|"listwise"|"pointwise",
  "system_prompt": "You are a helpful, harmless, and honest assistant
     evaluating responses...",
  "user_prompt_template": "### Instruction\n{prompt}\n\n
     ### Response A\n{response_a}\n\n
     ### Response B\n{response_b}\n\n
     Which response is better? Answer with A or B.",
  "output_parser": {
    "type": "regex",
    "pattern": "(?i)\\b([AB])\\b",
    "group": 1
  },
  "score_range": {"min":0, "max":1},   // for pointwise mode
  "tie_breaker": "random"|"prefer_a"|"prefer_b"|"reject_both"
}
```

POST /v1/judge-templates/{template_id}/calibrate
```
{
  "judge_model": ModelRef,                // or judge_endpoint
  "judge_endpoint": string?,
  "judge_api_model": string?,
  "calibration_set": DatasetRef,          // known-good preference pairs
                                          // (human-annotated)
  "n_trials": 100,
  "test_position_bias": true,             // swap A/B and check consistency
  "test_score_distribution": true,         // does it use full range?
  "test_tie_consistency": true             // same pair judged same way twice
}
```
-> 202 Accepted (async job)
On completion, calibration report:

```
{
  "template_id": "...",
  "status": "validated"|"degraded"|"unreliable",
  "metrics": {
    "agreement_with_human": 0.82,        // % matches with calibration set
    "position_bias_rate": 0.03,          // how often swapping A/B flips
                                          // the answer (lower is better)
    "score_distribution": {               // for pointwise
      "min": 0.1, "p25": 0.4, "median": 0.6, "p75": 0.8, "max": 0.95
    },
    "tie_consistency": 0.95,             // same pair judged same way
    "confidence_interval": [0.78, 0.86]
  },
  "recommendation": "Template is validated. Position bias is low (3%).
     Score distribution is healthy.  Safe to use for preference distillation.",
  "issues": []
}
```

If status is "unreliable" (agreement < 0.65 or position_bias > 0.15),
the template CANNOT be used in /v1/distill/preferences or
/v1/data/preference-pairs.  The API rejects with
ERR_JUDGE_NOT_CALIBRATED.

------------------------------------------------------------------------
10. TRAINING API
------------------------------------------------------------------------

Common request fields:
```
interface TrainingRequest {
  workspace_id:       string         // REQUIRED
  base_model:         ModelRef
  output_dir:         string
  datasets:           DatasetRef[]   // train + validation
  method:             string         // per endpoint
  hardware?:          HardwareSpec

  // NO fake unified hyperparameters block.
  // Instead, specify which backend and its native config.
  backend:            "trl"|"axolotl"|"unsloth"
  backend_config:     object         // backend-specific, see below

  peft?: {
    enabled:           boolean
    type?:             "lora"|"qlora"|"ia3"|"adalora"
    r?:                integer
    alpha?:            integer
    dropout?:          number
    target_modules?:   string[]
    bits?:             4|8
  }

  // Eval-during-training: run benchmarks on checkpoints,
  // auto-rollback to best, detect degradation.
  eval_during_training?: {
    enabled:           boolean
    benchmarks:        string[]      // e.g. ["gsm8k","hellaswag"]
    eval_every_n_steps: integer
    eval_subset_size?: integer       // limit samples for speed
    auto_rollback:     boolean       // revert to best checkpoint
                                     // if eval degrades
    rollback_threshold_pct: number   // default 10 (10% drop = rollback)
    early_stop_patience?: integer    // stop after N evals with no improvement
    save_best_metric:  string        // "eval/loss" | "gsm8k/exact_match"
    direction:         "min"|"max"   // min for loss, max for accuracy
  }

  // Monitoring overrides (defaults are sensible)
  monitoring?: {
    grad_norm_threshold?:    number  // default 5.0
    loss_spike_threshold?:   number  // default 3.0 (x multiplier)
    memory_fragmentation_alert_pct?: number  // default 30
    thermal_threshold_c?:    number  // default 85
    oom_threshold_pct?:      number  // default 95
  }

  wandb?: { "project":"...", "run_name":"..." }
  tensorboard?: { "log_dir":"..." }

  dry_run?:          boolean         // validate config, don't train
  cost_estimate_only?: boolean       // return estimate, don't queue
}
```

BACKEND CONFIG -- the honest approach.  Each backend has its own
config block matching its real capabilities.  The API documents
which fields each backend accepts and rejects unknown fields.

--- TRL backend_config ---
```
{
  // Matches TRL trainer kwargs + TrainingArguments
  "training_args": {
    "num_train_epochs": 3,
    "per_device_train_batch_size": 4,
    "learning_rate": 2e-5,
    "warmup_ratio": 0.03,
    "lr_scheduler_type": "cosine",
    "weight_decay": 0.01,
    "max_seq_length": 2048,
    "save_strategy": "epoch",
    "save_steps": 500,
    "eval_strategy": "steps",
    "eval_steps": 200,
    "logging_steps": 10,
    "seed": 42,
    "bf16": true,
    "gradient_checkpointing": true,
    "report_to": "wandb"
  },
  "trainer_kwargs": {
    "packing": false,
    "completion_only_loss": true,    // SFT only
    "dataset_text_field": "text"     // SFT raw-format only
  }
}
```

--- Axolotl backend_config ---
```
{
  // Matches axolotl YAML schema 1:1
  "sequence_len": 2048,
  "sample_packing": true,
  "pad_to_sequence_len": true,
  "micro_batch_size": 4,
  "gradient_accumulation_steps": 4,
  "num_epochs": 3,
  "learning_rate": 2e-5,
  "warmup_steps": 100,
  "lr_scheduler": "cosine",
  "optimizer": "adamw_torch",
  "weight_decay": 0.01,
  "save_strategy": "epoch",
  "flash_attention": true,
  "deepspeed": "deepspeed_configs/zero2.json",
  "datasets": [                       // overrides datasets in request
    { "path": "trl-lib/Capybara",
      "type": "chat_template",
      "chat_template": "chatml" }
  ]
}
```

--- Unsloth backend_config ---
```
{
  // Unsloth has a constrained model list.  The API validates
  // base_model against the supported list at submission time.
  // If unsupported: ERR_MODEL_NOT_SUPPORTED_BY_BACKEND
  "max_seq_length": 2048,
  "load_in_4bit": true,              // QLoRA
  "use_gradient_checkpointing": "unsloth",  // unsloth | true | false
  "full_finetune": false,            // true = no LoRA
  "trainer_type": "SFTTrainer"       // SFTTrainer | DPOTrainer | ...
}
```

The backend selection also drives validation:
  - Unsloth + unsupported model -> ERR_MODEL_NOT_SUPPORTED_BY_BACKEND
  - Axolotl + flash_attention on non-Flash model -> ERR_FEATURE_UNAVAILABLE
  - TRL + packing=true + completion_only_loss=true -> ERR_CONFIG_CONFLICT
    (TRL does not support both simultaneously)

========================= 10.1 SFT ==========================

POST /v1/train/sft

method = "sft"

TRL-specific fields in backend_config.trainer_kwargs:
  completion_only_loss, packing, dataset_text_field,
  instruction_template, response_template

Before training starts, the API checks:
  1. base_model has a chat_template (from HF config or expected_chat_template)
  2. dataset format matches (sft -> messages format, raw -> text field)
  3. tokenizer can handle max_seq_length
  4. no double-tokenization in dataset (DATASET_DETOKENIZED alert)
  If any check fails: ERR_CHAT_TEMPLATE_MISMATCH or
  ERR_DATASET_FORMAT_MISMATCH -- BEFORE GPUs are allocated.

==================== 10.2 DPO / KTO / ORPO ==================

POST /v1/train/dpo
POST /v1/train/kto
POST /v1/train/orpo

method = "dpo" | "kto" | "orpo"

TRL-specific fields:
```
{
  "trainer_kwargs": {
    "beta": 0.1,
    "label_smoothing": 0.0,
    "loss_type": "sigmoid",     // sigmoid | hinge | ipo | kto_pair
    "max_prompt_length": 512,
    "max_length": 1024,
    "reference_free": false,
    "precompute_ref_log_probs": true
  }
}
```

============ 10.3 PPO / GRPO (RL) ============================

POST /v1/train/ppo
POST /v1/train/grpo

method = "ppo" | "grpo"

Reward functions are referenced by ID (from §8), NOT inline code:

```
{
  "reward_function_ids": ["func-abc","func-def"],  // multiple = composite
  "reward_aggregation": "weighted_sum"|"mean"|"max",
  "reward_weights": [0.3, 0.7],                     // for weighted_sum

  // GRPO-specific
  "num_generations": 4,
  "max_new_tokens": 128,

  // PPO-specific
  "reward_model_id": "model-id",     // alternative to function_ids
  "kl_coef": 0.05,
  "cliprange": 0.2,
  "vf_coef": 0.1,
  "total_episodes": 10000,

  "advantage_whitening": "by_std"
}
```

If a reward_function_id does not exist or belongs to another
workspace: ERR_REWARD_FUNC_NOT_FOUND.

============== 10.4 Reward Model Training ====================

POST /v1/train/reward-model

method = "reward_model"
TRL-specific:
```
{
  "trainer_kwargs": {
    "loss_type": "bradley_terry",
    "num_labels": 1,
    "max_length": 1024
  }
}
```

============== 10.5 Full RLHF Pipeline ======================

POST /v1/train/rlhf   (orchestrated multi-stage pipeline)

```
{
  "workspace_id": "...",
  "base_model": ModelRef,
  "stages": [
    { "type": "sft",         "datasets": [DatasetRef],
      "backend": "trl", "backend_config": {...},
      "peft": {...}, "eval_during_training": {...} },
    { "type": "reward_model","datasets": [DatasetRef],
      "backend": "trl", "backend_config": {...} },
    { "type": "ppo",
      "reward_model_from_stage": 1,   // uses output of stage 1
      "datasets": [DatasetRef],
      "backend": "trl", "backend_config": {...} }
  ],
  "evaluate_after_each_stage": true,
  "fail_fast": true                   // stop pipeline if a stage fails
}
```
Returns a single parent job_id.  Each stage creates a child job
(parent_job_id set).  The WebSocket stream includes child job
status transitions.

------------------------------------------------------------------------
11. DISTILLATION API
------------------------------------------------------------------------

================ 11.1 Logit / KL Distillation =================

POST /v1/distill/logits

```
{
  "workspace_id":     "...",
  "teacher_model":     ModelRef,
  "student_model":     ModelRef,
  "dataset":           DatasetRef,         // prompt-only
  "output_dir":        string,
  "hyperparameters": {
    "temperature":     2.0,
    "kl_weight":       1.0,
    "hard_label_weight": 0.0,
    "max_length":      1024,
    "batch_size":      4,
    "learning_rate":   5e-5,
    "epochs":          3
  },
  "hardware":          HardwareSpec,
  "eval_during_training": { ... },
  "monitoring": { ... }
}
```

============= 11.2 Response Distillation (Synthetic SFT) ========

POST /v1/distill/responses

```
{
  "workspace_id":     "...",
  "teacher_model":     ModelRef,          // OR teacher_endpoint
  "teacher_endpoint":  string?,
  "teacher_api_model": string?,
  "student_model":     ModelRef,
  "prompt_dataset":    DatasetRef,
  "output_sft_dataset": string,            // registered dataset id

  "generation": {
    "max_new_tokens":  512,
    "temperature":     0.7,
    "n_per_prompt":    1,
    "batch_size":      32,
    "dedup":           true,
    "min_length":      50,
    "max_length":      2048
  },

  // Quality filter on generated outputs BEFORE creating SFT dataset
  "quality_filter": {
    "enabled": true,
    "judge_model": ModelRef?,              // or endpoint
    "min_score": 0.5,
    "reject_patterns": ["I cannot", "As an AI", "I'm unable to"],
    "language_filter": ["en"],
    "perplexity_filter": { "model": ModelRef, "max": 50 }
  },

  "then_train_sft": {
    "enabled":          true,
    "output_dir":       string,
    "backend":          "trl"|"axolotl"|"unsloth",
    "backend_config":   { ... },
    "peft": { ... },
    "eval_during_training": { ... }
  },

  "hardware": HardwareSpec
}
```
Returns a single job that runs: generate -> filter -> validate -> (train).
Each phase is a child job with its own metrics stream.

=========== 11.3 Preference Distillation (Synthetic DPO) ======

POST /v1/distill/preferences

```
{
  "workspace_id":     "...",
  "judge_model":       ModelRef,          // OR judge_endpoint
  "judge_endpoint":    string?,
  "judge_api_model":   string?,
  "judge_template_id": string,            // REQUIRED, must be calibrated
  "student_model":     ModelRef,
  "prompt_dataset":    DatasetRef,
  "output_pref_dataset": string,

  "generation": {
    "n_per_prompt":    2,                 // >=2 candidates to rank
    "max_new_tokens":  512,
    "temperature":     0.8                // need diversity
  },

  "judge_mode":        "pairwise"|"listwise"|"pointwise",
  "ranking_method":    "bradley_terry"|"elo"|"margin",
  "filter": {
    "min_score_gap":   0.1,
    "drop_ties":       true
  },

  "then_train_dpo": {
    "enabled":          true,
    "output_dir":       string,
    "backend":          "trl",
    "backend_config":   { ... },
    "peft": { ... },
    "eval_during_training": { ... }
  }
}
```

If judge_template_id is not calibrated or status was "unreliable":
  ERR_JUDGE_NOT_CALIBRATED with recovery_hint pointing to
  /v1/judge-templates/{template_id}/calibrate.

------------------------------------------------------------------------
12. QUANTIZATION API
------------------------------------------------------------------------

POST /v1/quantize

```
{
  "workspace_id":     "...",
  "model":             ModelRef,
  "output_dir":        string,
  "method":            "gguf"|"awq"|"gptq"|"bitsandbytes"|"fp8",
  "bits":              4|8,
  "gguf_quant":        "Q4_K_M"|"Q5_K_M"|"Q6_K"|"Q8_0"|"IQ2_XXS",
  "group_size":        128,
  "calibration_dataset": DatasetRef,
  "calibration_samples": 128,
  "format":            "gguf"|"safetensors"|"auto",
  "target":            "vllm"|"llamacpp"|"transformers",
  "trust_remote_code": false
}
```

Validation before running:
  - AWQ/GPTQ require calibration_dataset -> ERR_CALIBRATION_REQUIRED
  - calibration_dataset domain should match target use case
    (if mismatch detected via embedding distance: warning
     ERR_QUANT_CALIBRATION_DRIFT, not fatal)
  - GGUF quant label must be valid for method=gguf

Also: POST /v1/models/{model_id}/quantize (shorthand)

------------------------------------------------------------------------
13. MERGING API
------------------------------------------------------------------------

POST /v1/merge

```
{
  "workspace_id":     "...",
  "models": [
    { "model_id": "...", "weight": 0.5, "adapter_id": "..."? },
    { "model_id": "...", "weight": 0.5 }
  ],
  "method":         "linear"|"ties"|"dare_ties"|"slerp"|"task_arithmetic",
  "slerp_t":        0.5,            // slerp only
  "density":        0.5,            // DARE
  "int8_mask":      true,           // DARE-TIES
  "output_dir":     string,
  "output_name":    string
}
```

Validation:
  - All models must have the same architecture (hidden_size, num_layers,
    vocab_size).  If not: ERR_MERGE_ARCHITECTURE_MISMATCH with details
    showing which dimension differs.
  - Adapter merge: adapter rank must match base model's expected
    LoRA shape.  If not: ERR_LORA_MERGE_SHAPE_MISMATCH.

------------------------------------------------------------------------
14. INFERENCE & SERVING API
------------------------------------------------------------------------

============ 14.1 Deploy / Serve ===========================

POST /v1/serve

```
{
  "workspace_id":     "...",
  "model":             ModelRef,
  "engine":            "vllm"|"llamacpp"|"transformers",
  "port":              8000,
  "host":              "0.0.0.0",
  "max_model_len":     8192,
  "tensor_parallel_size":   1,
  "gpu_memory_utilization": 0.9,
  "enable_prefix_caching":  true,
  "enable_chunked_prefill": false,
  "quantization":     "awq"|"gptq"|"fp8"|"none",
  "speculative": {
    "draft_model":     ModelRef?,
    "num_speculative_tokens": 5
  },
  "max_num_seqs":     256,
  "trust_remote_code": false
}
```

The scheduler checks GPU availability.  If GPUs are occupied by
training jobs and preempt_serving_for_training is false, the serve
job enters "queued".  If preempt_serving_for_training is true and
a training job needs the GPUs, this serve job gets preempted
(status -> "paused", endpoint_url cleared, GPUs released).

GET    /v1/serve/{job_id}              -> includes endpoint_url when up
DELETE /v1/serve/{job_id}              -> tear down

============ 14.2 Chat Completions  ========================

POST /v1/serve/{job_id}/chat/completions   (OpenAI-compatible)
POST /v1/serve/{job_id}/completions
GET  /v1/serve/{job_id}/models
GET  /v1/serve/{job_id}/metrics             (Prometheus)

============ 14.3 Structured / Constrained Gen ============

POST /v1/serve/{job_id}/structured

```
{
  "prompt":       "...",                // OR messages
  "messages":     [...],
  "constraint": {
    "type":       "json_schema"|"regex"|"choice"|"grammar",
    "json_schema": {...}?,
    "pattern":    "..."?,
    "choices":    [...]?,
    "grammar":    "..."?                // EBNF / GBNF
  },
  "max_tokens":   512,
  "temperature":  0.0,
  "backend":       "outlines"|"xgrammar"|"llama_grammar"
}
```

Response:
```
{
  "content": "...",
  "parsed": { ... }?,
  "coercion_notes": [                    // NEW in v2
    {
      "field": "age",
      "issue": "value_forced",
      "detail": "Model produced -5; schema constrains int >= 0.
                 Output forced to nearest valid token path."
    }
  ]?,
  "usage": { "prompt_tokens": N, "completion_tokens": M },
  "constraint_backend": "outlines",
  "constraint_guaranteed": true          // structure is valid
}
```

coercion_notes is populated when the grammar engine had to override
the model's preferred token.  This does NOT mean the output is
semantically correct -- only structurally valid.  Clients should
check coercion_notes for quality-sensitive applications.

============ 14.4 Embeddings ==============================

POST /v1/serve/{job_id}/embeddings         (OpenAI-compatible)
POST /v1/serve/{job_id}/embeddings/batch

------------------------------------------------------------------------
15. EVALUATION API
------------------------------------------------------------------------

POST /v1/eval

```
{
  "workspace_id":     "...",
  "model":             ModelRef,          // OR served_endpoint
  "served_endpoint":  string?,
  "tasks":            ["mmlu","gsm8k","hellaswag","truthfulqa",
                       "arc_challenge","human_eval"],
  "custom_tasks":     [{ "name":"...", "dataset": DatasetRef,
                          "metric":"exact_match",
                          "fewshot":0 }],
  "num_fewshot":      5,
  "batch_size":       "auto"|"8",
  "limit":            null,               // restrict N samples
  "device":           "cuda:0",
  "backend":          "hf"|"vllm",
  "output_path":      "results/run-001",
  "log_samples":      true,
  "confirm_unsafe_code": false            // required true for HumanEval
}
```

GET    /v1/eval/compare?runs=run-001,run-002
POST   /v1/eval/custom-task
GET    /v1/eval/tasks                      -> list all available tasks

Before eval starts, the API checks for train/eval data leakage:
  - If the model's training dataset content_hash overlaps with the
    eval task's test set (>1% prompt overlap): ERR_EVAL_LEAKAGE
    with details.  This is a WARNING-level check (eval still runs,
    but the result is flagged in the output).

------------------------------------------------------------------------
16. DATA CURATION API
------------------------------------------------------------------------

============ 16.1 Synthetic Data Generation ===============

POST /v1/data/synthesize

```
{
  "workspace_id":     "...",
  "generator_model":   ModelRef,         // OR generator_endpoint
  "generator_endpoint": string?,
  "generator_api_model": string?,
  "seed_prompts":      DatasetRef,
  "mode":             "self_instruct"|"evol_instruct"|"magpie"|
                       "backtranslation",
  "evol_methods":     ["simplify","elaborate","add_constraint"],
  "n_instructions":   1000,
  "max_instruction_length": 256,
  "max_response_length": 512,
  "dedup":            true,
  "dedup_method":     "minhash"|"semantic",
  "min_quality_score": 0.5,
  "output_dataset":   string,
  "quality_filter": { ... }              // same as distill/responses
}
```

============ 16.2 Dataset Filtering & Dedup ==============

POST /v1/data/filter
POST /v1/data/dedup
POST /v1/data/quality-score

```
POST /v1/data/filter
{
  "workspace_id": "...",
  "input":  DatasetRef,
  "output": string,
  "rules": [
    { "type":"length",          "min":50, "max":4096 },
    { "type":"language",        "langs":["en","fr"] },
    { "type":"perplexity",      "model":"...", "max":50 },
    { "type":"keyword_blocklist","terms":["..."] },
    { "type":"pii_redact",      "entities":["email","phone","ssn"] },
    { "type":"toxicity",        "threshold":0.8 },
    { "type":"decontamination", "benchmark":"mmlu", "overlap_window":8 }
  ]
}
```

```
POST /v1/data/dedup
{
  "workspace_id": "...",
  "input":   DatasetRef,
  "output":  string,
  "method":  "minhash"|"exact"|"embedding",
  "threshold": 0.8,
  "ngram":   5,
  "embedding_model": ModelRef?
}
```

```
POST /v1/data/quality-score
{
  "workspace_id": "...",
  "input":   DatasetRef,
  "model":   ModelRef,
  "output":  string,
  "metrics": ["instruction_following","verbosity","coherence",
              "safety","format"]
}
```

============ 16.3 Preference Pair Generation ==============

POST /v1/data/preference-pairs

```
{
  "workspace_id":     "...",
  "student_model":     ModelRef,
  "judge_model":       ModelRef?,        // OR judge_endpoint
  "judge_endpoint":    string?,
  "judge_api_model":   string?,
  "judge_template_id": string,            // REQUIRED, must be calibrated
  "prompt_dataset":    DatasetRef,
  "n_per_prompt":      2,
  "generation":        { "max_new_tokens":512, "temperature":0.8 },
  "judge_mode":        "pairwise",
  "ranking_method":    "bradley_terry"|"elo",
  "output_dataset":    string
}
```

------------------------------------------------------------------------
17. TOKENIZATION & CONTEXT MANAGEMENT API
------------------------------------------------------------------------

POST /v1/tokenize
POST /v1/detokenize
POST /v1/context/compress
POST /v1/context/chunk
GET  /v1/models/{model_id}/tokenizer-info

```
POST /v1/context/compress
{
  "text":     "...",
  "model":    ModelRef,
  "rate":     0.5,
  "method":   "llmlingua"|"llmlingua2",
  "force_tokens": ["\n","Question:","Answer:"]
}
-> { "compressed_text":"...", "original_tokens":2048,
     "compressed_tokens":1024, "ratio":0.50 }

POST /v1/context/chunk
{
  "text":       "...",
  "model":      ModelRef,
  "chunk_size": 1024,
  "overlap":    128,
  "strategy":   "fixed"|"semantic"|"recursive",
  "separators": ["\n\n", "\n", ". "]
}
-> { "chunks": ["...", "..."], "n": 12 }
```

------------------------------------------------------------------------
18. ERROR CODES
------------------------------------------------------------------------

  -- Generic --
  ERR_BAD_REQUEST              400  malformed request
  ERR_UNAUTHORIZED             401  auth failed
  ERR_FORBIDDEN                 403  quota / access denied
  ERR_NOT_FOUND                 404  model / dataset / job not found
  ERR_CONFLICT                  409  resource already in use
  ERR_UNPROCESSABLE             422  valid request, unsupported combo
  ERR_RATE_LIMITED              429
  ERR_INTERNAL                  500  server-side fault
  ERR_HARDWARE_UNAVAILABLE      503  no GPUs free
  ERR_APPROVAL_REQUIRED         402  cost above threshold, needs approval

  -- Model / Backend --
  ERR_CHAT_TEMPLATE_MISMATCH    -- model chat template doesn't match
                                     dataset format.  recovery_hint:
                                     specify expected_chat_template or
                                     use a custom_chat_template.
  ERR_DATASET_FORMAT_MISMATCH   -- dataset format doesn't match method
                                     (e.g. sft format for DPO)
  ERR_MODEL_NOT_SUPPORTED_BY_BACKEND
                                -- e.g. Unsloth doesn't support this
                                     model.  recovery_hint: use TRL or
                                     Axolotl backend, or choose a
                                     supported model.
  ERR_CONFIG_CONFLICT           -- e.g. TRL packing + completion_only_loss
                                     can't both be true.
  ERR_FEATURE_UNAVAILABLE       -- e.g. flash_attention on non-Flash
                                     model with Axolotl backend.
  ERR_LORA_MERGE_SHAPE_MISMATCH -- adapter rank/shape doesn't match
                                     base model.
  ERR_MERGE_ARCHITECTURE_MISMATCH
                                -- models in merge have different
                                     architecture dims.

  -- Training --
  ERR_LR_WARMUP_EXPLODED        -- loss > 10x initial in first 50 steps.
                                     recovery_hint: reduce learning_rate
                                     or increase warmup_ratio.
  ERR_DATASET_DETOKENIZED       -- token length distribution suggests
                                     double-tokenization.  recovery_hint:
                                     check dataset preprocessing pipeline.
  ERR_REWARD_FUNC_NOT_FOUND     -- reward_function_id doesn't exist or
                                     belongs to another workspace.
  ERR_GRAD_NORM_EXPLODED        -- gradient norm exceeded threshold.
                                     recovery_hint: reduce LR, enable
                                     gradient clipping, check data.
  ERR_LOSS_NAN                  -- loss is NaN/Inf.  auto-rolled back to
                                     last good checkpoint.
  ERR_OOM                       -- out of memory.  recovery_hint:
                                     reduce batch_size, max_seq_length,
                                     or enable gradient_checkpointing.

  -- Quantization --
  ERR_QUANT_CALIBRATION_DRIFT   -- calibration dataset domain doesn't
                                     match target use case (warning).
  ERR_CALIBRATION_REQUIRED      -- AWQ/GPTQ requires calibration_dataset.
  ERR_QUANTIZATION_FAILED       -- quant backend failed.

  -- Eval --
  ERR_EVAL_LEAKAGE              -- train/eval prompt overlap > 1%.
                                     (warning, eval still runs)
  ERR_EVAL_TASK_UNKNOWN         -- benchmark name not recognized.

  -- Judge / Distillation --
  ERR_JUDGE_NOT_CALIBRATED      -- judge_template_id not calibrated or
                                     calibration status was "unreliable".
                                     recovery_hint: run
                                     /v1/judge-templates/{id}/calibrate.

  -- Dataset --
  ERR_DATASET_HASH_MISMATCH     -- dataset content_hash changed since
                                     last reference.  A model was
                                     trained on a different version
                                     than currently registered.

  -- Checkpoint --
  ERR_CHECKPOINT_CORRUPTION     -- shard write failed or checkpoint
                                     is incomplete.  recovery_hint:
                                     retry, check disk space.

  -- Serving --
  ERR_SERVE_PREEMPTED           -- serve job was preempted by scheduler
                                     for higher-priority training job.

  -- Structured Generation --
  ERR_CONSTRAINT_VIOLATION      -- structured-gen schema failed (should
                                     not happen with grammar enforcement;
                                     indicates a backend bug).

All errors include retryable + recovery_hint where applicable.
Clients should retry on: 429, 503, ERR_INTERNAL(retryable=true),
ERR_OOM (with reduced batch_size), ERR_LR_WARMUP_EXPLODED (with
reduced LR).  Do NOT retry on: ERR_CHAT_TEMPLATE_MISMATCH,
ERR_DATASET_FORMAT_MISMATCH, ERR_CONFIG_CONFLICT (config changes
needed).

------------------------------------------------------------------------
19. EXAMPLES
------------------------------------------------------------------------

--- Example A: SFT with eval-during-training + auto-rollback ---------

```
POST /v1/train/sft
{
  "workspace_id": "ws-prod",
  "base_model": {
    "model_id": "Qwen/Qwen2.5-0.5B",
    "revision": "a1b2c3d",
    "expected_chat_template": "chatml"
  },
  "output_dir": "qwen-capybara-sft",
  "datasets": [
    { "dataset_id": "trl-lib/Capybara", "split": "train",
      "format": "sft", "content_hash": "sha256:9f4a..." }
  ],
  "method": "sft",
  "backend": "trl",
  "backend_config": {
    "training_args": {
      "num_train_epochs": 3,
      "per_device_train_batch_size": 4,
      "learning_rate": 2e-5,
      "warmup_ratio": 0.03,
      "lr_scheduler_type": "cosine",
      "bf16": true,
      "gradient_checkpointing": true,
      "save_strategy": "steps",
      "save_steps": 500,
      "logging_steps": 10
    },
    "trainer_kwargs": {
      "completion_only_loss": true,
      "packing": false
    }
  },
  "peft": {
    "enabled": true, "type": "lora",
    "r": 16, "alpha": 32,
    "target_modules": ["q_proj","v_proj"]
  },
  "eval_during_training": {
    "enabled": true,
    "benchmarks": ["gsm8k","hellaswag"],
    "eval_every_n_steps": 500,
    "eval_subset_size": 200,
    "auto_rollback": true,
    "rollback_threshold_pct": 10,
    "save_best_metric": "gsm8k/exact_match",
    "direction": "max"
  },
  "monitoring": {
    "grad_norm_threshold": 5.0,
    "memory_fragmentation_alert_pct": 30
  },
  "wandb": { "project": "qwen-finetune", "run_name": "capybara-sft-v1" }
}

-> 202 Accepted
{
  "job_id": "f3c1...e9a",
  "type": "sft", "status": "queued",
  "workspace_id": "ws-prod",
  "cost_estimate": {
    "estimated_gpu_hours": 4.5,
    "estimated_cost_usd": 18.00,
    "confidence": "high"
  },
  "logs_url": "wss://api.../v1/jobs/f3c1...e9a/logs",
  "metrics_url": "wss://api.../v1/jobs/f3c1...e9a/metrics"
}
```

--- Example B: GRPO with registered reward functions ----------------

```
# 1. Register reward functions
POST /v1/reward-functions
{
  "name": "code-format",
  "type": "builtin",
  "builtin": {
    "kind": "format",
    "pattern": "```python.*```",
    "reward_on_match": 1.0,
    "reward_on_miss": 0.0
  }
}
-> { "func_id": "func-abc" }

POST /v1/reward-functions
{
  "name": "unique-words",
  "type": "sandboxed_python",
  "code": "def reward(prompts, completions, **kwargs):\n
              return [len(set(c.split())) / 100.0 for c in completions]",
  "allowed_builtins": ["len","set","str","float","list"]
}
-> { "func_id": "func-def" }

# 2. Train
POST /v1/train/grpo
{
  "workspace_id": "ws-prod",
  "base_model": { "model_id": "Qwen/Qwen2.5-0.5B-Instruct" },
  "output_dir": "qwen-grpo",
  "datasets": [
    { "dataset_id": "trl-lib/tldr", "split": "train",
      "format": "prompt_only" }
  ],
  "method": "grpo",
  "backend": "trl",
  "backend_config": {
    "training_args": {
      "num_train_epochs": 1,
      "per_device_train_batch_size": 4,
      "learning_rate": 1e-5,
      "bf16": true
    }
  },
  "reward_function_ids": ["func-abc","func-def"],
  "reward_aggregation": "weighted_sum",
  "reward_weights": [0.7, 0.3],
  "num_generations": 4,
  "max_new_tokens": 128
}
```

--- Example C: Judge calibration, then preference distillation ------

```
# 1. Register judge template
POST /v1/judge-templates
{
  "workspace_id": "ws-prod",
  "name": "my-constitutive-v1",
  "mode": "pairwise",
  "system_prompt": "You are an evaluator...",
  "user_prompt_template": "### Instruction\n{prompt}\n\n
     ### Response A\n{response_a}\n\n
     ### Response B\n{response_b}\n\n
     Which response is better? Answer A or B.",
  "output_parser": { "type":"regex", "pattern":"(?i)\\b([AB])\\b", "group":1 },
  "tie_breaker": "reject_both"
}
-> { "template_id": "tpl-xyz" }

# 2. Calibrate
POST /v1/judge-templates/tpl-xyz/calibrate
{
  "judge_model": { "model_id": "Qwen/Qwen2.5-72B-Instruct" },
  "calibration_set": { "dataset_id":"human-prefs-gold","split":"test",
                       "format":"preference" },
  "n_trials": 100,
  "test_position_bias": true
}
-> 202 Accepted (async)
# ... on completion ...
{
  "template_id": "tpl-xyz",
  "status": "validated",
  "metrics": {
    "agreement_with_human": 0.84,
    "position_bias_rate": 0.04,
    "tie_consistency": 0.93
  },
  "recommendation": "Template is validated. Safe to use."
}

# 3. Run preference distillation
POST /v1/distill/preferences
{
  "workspace_id": "ws-prod",
  "judge_model": { "model_id": "Qwen/Qwen2.5-72B-Instruct" },
  "judge_template_id": "tpl-xyz",
  "student_model": { "model_id": "Qwen/Qwen2.5-7B-Instruct" },
  "prompt_dataset": { "dataset_id":"my-prompts","format":"prompt_only" },
  "output_pref_dataset": "student-prefs-v1",
  "generation": { "n_per_prompt": 2, "max_new_tokens": 512,
                  "temperature": 0.8 },
  "judge_mode": "pairwise",
  "ranking_method": "bradley_terry",
  "filter": { "min_score_gap": 0.1, "drop_ties": true },
  "then_train_dpo": {
    "enabled": true,
    "output_dir": "student-dpo-v1",
    "backend": "trl",
    "backend_config": {
      "training_args": { "learning_rate": 5e-7,
                         "per_device_train_batch_size": 4,
                         "bf16": true },
      "trainer_kwargs": { "beta": 0.1, "max_prompt_length": 512,
                          "max_length": 1024 }
    }
  }
}
```

--- Example D: Cost-estimated job that needs approval ----------------

```
POST /v1/train/rlhf
{
  "workspace_id": "ws-prod",
  "base_model": { "model_id": "meta-llama/Llama-3-70B" },
  "stages": [...],
  "evaluate_after_each_stage": true
}

-> 202 Accepted
{
  "job_id": "rlhf-001",
  "status": "queued",
  "approval_required": true,
  "cost_estimate": {
    "estimated_gpu_hours": 240,
    "estimated_cost_usd": 1920.00,
    "confidence": "medium"
  },
  "approval": {
    "approval_id": "appr-001",
    "status": "pending"
  },
  "message": "Job requires approval (estimated cost $1920 exceeds
              workspace threshold $500).  No GPUs allocated until
              approved."
}

# Approve
POST /v1/approvals/appr-001/approve
{ "reason": "Approved for production RLHF run Q3" }
-> { "status": "approved", "approver_id": "user-admin" }

# Job proceeds to scheduling
GET /v1/jobs/rlhf-001
-> { "status": "starting", "message": "GPUs assigned to 4x A100-80GB" }
```

--- Example E: Structured gen with coercion notes -------------------

```
POST /v1/serve/{job_id}/structured
{
  "messages": [{"role":"user","content":"Extract person: Alice, age -5"}],
  "constraint": {
    "type": "json_schema",
    "json_schema": {
      "type":"object",
      "properties": {
        "name": {"type":"string"},
        "age": {"type":"integer","minimum":0}
      },
      "required":["name","age"]
    }
  },
  "max_tokens": 128,
  "temperature": 0.0,
  "backend": "outlines"
}

-> {
  "content": "{\"name\":\"Alice\",\"age\":0}",
  "parsed": { "name":"Alice", "age":0 },
  "coercion_notes": [
    {
      "field": "age",
      "issue": "value_forced",
      "detail": "Model produced -5 but schema constrains minimum 0.
                 Grammar forced the output to 0 (nearest valid token
                 path).  Check if the input was erroneous."
    }
  ],
  "constraint_backend": "outlines",
  "constraint_guaranteed": true,
  "usage": { "prompt_tokens": 22, "completion_tokens": 15 }
}
```

--- Example F: Dataset validation catches real problems --------------

```
POST /v1/datasets/ds-abc123/validate
{
  "expected_format": "sft",
  "tokenizer_id": "Qwen/Qwen2.5-0.5B",
  "checks": ["schema","token_stats","duplicate_scan",
              "detokenization","pii_scan","benchmark_overlap"],
  "benchmark_names": ["mmlu","gsm8k"]
}

-> {
  "valid": false,
  "issues": [
    { "check":"detokenization", "severity":"critical",
      "message":"Median token count 920 is 2.1x expected 440.
                 Data may be double-tokenized.",
      "affected_rows_pct": 38.2 },
    { "check":"benchmark_overlap", "severity":"warning",
      "message":"12 prompts (0.1%) overlap with gsm8k test set.",
      "affected_rows": 12 },
    { "check":"pii_scan", "severity":"warning",
      "message":"8 rows contain email addresses.",
      "affected_rows": 8 }
  ]
}
```

--- Example G: Eval-during-training catches degradation --------------

```
// WebSocket stream for a training job:
{ "type":"eval_checkpoint", "job_id":"...", "step":500,
  "checkpoint_id":"ckpt-500",
  "benchmarks": { "gsm8k":{"exact_match":0.35},
                  "hellaswag":{"acc_norm":0.77} },
  "is_best_so_far": true }

{ "type":"eval_checkpoint", "job_id":"...", "step":1000,
  "checkpoint_id":"ckpt-1000",
  "benchmarks": { "gsm8k":{"exact_match":0.41},
                  "hellaswag":{"acc_norm":0.79} },
  "is_best_so_far": true }

{ "type":"alert", "job_id":"...", "severity":"warning",
  "alert_code":"EVAL_DEGRADATION",
  "message":"gsm8k/exact_match dropped from 0.41 to 0.28 (31% drop)
              at step 1500.  Threshold is 10%.",
  "auto_action_taken": "rollback_to_best_checkpoint",
  "recommended_action":"Consider early stopping. Best checkpoint
                         is ckpt-1000 (step 1000)." }

{ "type":"status", "job_id":"...", "status":"running",
  "message":"Rolled back to ckpt-1000. Training continues from
              best checkpoint.",
  "step": 1000 }
```

--- Example H: Alert-driven recovery (grad norm explosion) -----------

```
{ "type":"alert", "job_id":"...", "severity":"critical",
  "alert_code":"GRAD_NORM_EXPLODING",
  "message":"Gradient norm 15.2 > threshold 5.0 at step 2304.
              Auto-applied gradient clipping to 1.0.",
  "auto_action_taken":"gradient_clipping_applied",
  "recommended_action":"If this recurs, reduce learning_rate
                         from 2e-5 to 1e-5." }

// User follows the recommendation:
POST /v1/jobs/{job_id}/pause
POST /v1/jobs/{job_id}/cancel   // (can't change LR mid-run;
                                 //  must resubmit)

// Resubmit with lower LR
POST /v1/train/sft
{ ..., "backend_config": {
    "training_args": { "learning_rate": 1e-5, ... } } }
```

========================================================================
END OF SPEC v2.0
========================================================================