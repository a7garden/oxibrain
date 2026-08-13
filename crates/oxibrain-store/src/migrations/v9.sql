-- v9: uncertainty_json column on episodes (§13.1, P10, D23).
--
-- Derived episodes (consolidation, community summaries) carry an Uncertainty
-- struct as JSON — contradiction_rate, single_source_fraction, staleness,
-- trust_exclusion_fraction. P10: "compression may lose detail, never doubt."
-- Primary episodes have NULL (they are not compressed).
ALTER TABLE episodes ADD COLUMN uncertainty_json TEXT;
