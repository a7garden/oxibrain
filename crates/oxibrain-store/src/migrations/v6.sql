-- v6: dual FTS index — word + trigram (§7.4). Porter stemmer removed (F22).
-- Both tables are always populated; RRF fuses them. No script detection, no
-- routing (P11). On English the word index dominates; on CJK the trigram
-- index carries the channel; on mixed text both contribute.

-- Drop the v1/v3 single FTS table.
DROP TABLE IF EXISTS episodes_fts;

-- Word-level index: unicode61 tokenizer (no stemmer — porter removed, F22).
CREATE VIRTUAL TABLE IF NOT EXISTS fts_word USING fts5(
    body,
    space_id    UNINDEXED,
    target_kind UNINDEXED,
    target_id   UNINDEXED,
    tokenize = 'unicode61'
);

-- Trigram index: handles scripts without word boundaries (CJK, Thai, etc.).
CREATE VIRTUAL TABLE IF NOT EXISTS fts_ngram USING fts5(
    body,
    space_id    UNINDEXED,
    target_kind UNINDEXED,
    target_id   UNINDEXED,
    tokenize = 'trigram'
);
