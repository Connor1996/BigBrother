# Prompt Templates

BigBrother now loads its agent prompts from the fixed Markdown files in this folder.

These files are the built-in defaults:

- `actionable.md`: main prompt for CI failures, merge conflicts, and review-feedback runs
- `deep_review.md`: main prompt for manual deep review runs
- `ci_failure_rules.md`: CI-only rule block inserted into `actionable.md`
- `workspace_ready.md`: extra operator guidance when BigBrother prepared a clean checkout
- `resumed_conflict.md`: extra operator guidance when BigBrother resumes an unresolved conflict workspace
- `deep_review_artifact.md`: extra operator guidance that tells the agent where to write the deep review artifact

BigBrother looks for these templates at the fixed path `./prompts/*.md` next to
`symphony-rs.toml`. To customize prompts on a machine, edit these files directly.

Available placeholders:

- `actionable.md`: `{{trigger}}`, `{{repo}}`, `{{number}}`, `{{url}}`, `{{title}}`, `{{base_ref}}`, `{{base_sha}}`, `{{head_ref}}`, `{{head_sha}}`, `{{ci_status}}`, `{{review_status}}`, `{{mergeable_state}}`, `{{has_conflicts}}`, `{{review_comments}}`, `{{issue_comments}}`, `{{reviewer_activity}}`, `{{body}}`, `{{trigger_specific_rules}}`, `{{additional_instructions_block}}`
- `deep_review.md`: `{{trigger}}`, `{{repo}}`, `{{number}}`, `{{url}}`, `{{title}}`, `{{base_ref}}`, `{{base_sha}}`, `{{head_ref}}`, `{{head_sha}}`, `{{ci_status}}`, `{{review_status}}`, `{{mergeable_state}}`, `{{body}}`, `{{additional_instructions_block}}`
- `deep_review_artifact.md`: `{{target_dir}}`, `{{file_name}}`, `{{artifact_path}}`

`{{trigger_specific_rules}}` and `{{additional_instructions_block}}` can expand to an empty string, so leave sensible spacing around them in custom templates.
