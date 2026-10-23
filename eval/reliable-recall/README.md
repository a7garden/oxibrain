# Reliable-recall evaluation

`development.jsonl` is the diagnostic partition. Its first run exposed a misconfigured document space and a common-term fusion failure. It was used for development and is not held-out evidence.

`heldout.jsonl` is a frozen synthetic verification partition: 200 questions in 40 trajectories, five writing-system classes, four positive categories (160 questions), and 40 questions with no supporting answer. UTF-8 gold ranges identify the required relation in the correct source; an identical answer in a rejected staging example does not receive support credit. The partitions have disjoint trajectory labels and answer vocabulary but share templates. They are not independent human annotations, real-user tasks, or external benchmarks.

The artifact pin, tokenizer revision and hashes are in `manifest.json`. The runner validates gold boundaries, creates isolated temporary stores and canonical PDC test documents, checks that source files remain unchanged, and independently recounts the canonical context with the actual tokenizer. It runs at 2,000 and 3,000 tokens. `results/source-support.json` contains per-question results, source-support fractions, grouped paired intervals and costs. The historical baseline is the retained legacy context assembler under the same artifact and budget; it has no source-range receipts, so support is credited only for complete verbatim gold text with matching layer provenance.

```sh
OXIBRAIN_TOKENIZER_JSON=/absolute/path/tokenizer.json \
OXIBRAIN_EVAL_OUTPUT=/absolute/path/recall-eval.json \
target/release/oxibrain admin eval --suite recall
```

This measures source delivery. It does not grade an answer or abstention. The 40 no-answer cases are excluded from Full Support denominators; their offline per-category zero values mean no positive annotations, not failed reader abstention. The 95% paired trajectory bootstrap describes these synthetic samples; shared templates limit independence and generalization.

For live reader evaluation, create an explicit configuration such as:

```json
{
  "corpus": "eval/reliable-recall/heldout.jsonl",
  "tokenizer": "/absolute/path/tokenizer.json",
  "output": "recall-live.json",
  "live_reader": true,
  "reader_revision": "explicit-pinned-reader-revision",
  "max_cases": 200
}
```

```sh
OXIBRAIN_EVAL_CONFIG=/absolute/path/eval-config.json \
OXIBRAIN_LLM_PROVIDER=loopback \
OXIBRAIN_LLM_MODEL=explicit-model-id \
target/release/oxibrain admin eval --suite full
```

The provider must be explicitly selected; ambient provider credentials alone cannot activate this suite. A local model digest or explicit reader revision is required. A full 200-question run makes 1,600 reader calls (two budgets, four matched arms: retrieved, legacy, gold, no evidence), then runs the three existing extraction fixtures with the live provider. Reader requests/responses and diagnostic citation-aware exact-match scores are captured in the report; extraction responses, triples and precision/recall are saved beside it with extension `.extraction.json`. Extraction captures can be replayed offline with `OXIBRAIN_EVAL_REPLAY=/absolute/path/capture.extraction.json ... admin eval --suite fast`. No live reader run was performed for this implementation.

The golden and experience manifests pin `read_at`; the recall arm passes each question's explicit `as_of`. The existing `fast`, `gate`, `experience` and `experiential` suites keep their fixture/reachability labels. Public LongMemEval/LoCoMo, a lexical+dense control with a real embedder, model reranking, and supported-answer quality claims remain separate experiments.
