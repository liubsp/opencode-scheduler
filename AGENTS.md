# Agent guidance

## Scope and workflow

- OpenCode V2 only. Verify the running service contract; the CLI and service versions may differ.
- Complete M1: manual one-shot prompt scheduling through a Rust background server, globally installed CLI, and thin plugin, including distribution and installed validation. The human reauthorized the remaining M1 tasks after the initial CLI pilot; do not build later automatic-rule milestones without authorization.
- Normal user commands are `opencode-scheduler ...` with human-readable output. Keep JSON as an optional automation interface, not the primary documented workflow.
- Astra authors designs only. Use inherited Sol for planning, all reviews, implementation, testing, and administration.
- Keep process documents under ignored `.superpowers/` and temporary work under ignored `.tmp/`.
- Follow work through the real installed user-facing path. Report automated, installation, and live evidence separately.
- Live prompt tests may target only the unambiguously resolved existing `Dummy session`. Do not restart the user's OpenCode service or alter machine startup, clock, or power settings for testing.
- Run formatting, static checks, tests, and package/privacy checks before committing a milestone. Preserve unrelated configuration during installation.

## License and IP policy

This project follows the existing plugin suite's MIT licensing. Dependencies and incorporated code must use permissive or public-domain licenses compatible with distribution under this policy; preserve required notices. Do not incorporate copyleft or proprietary runtime code without approval. Tools used only for development are not shipped dependencies.

Use factual product and trademark names where needed for interoperability and documentation. Do not include borrowed logos, brand artwork, or other third-party assets without permission and a compatible license.

## Privacy and implementation

- Never commit credentials, private identities, session content, local endpoints, absolute paths, or raw diagnostics. Keep runtime data outside the checkout.
- Remap compiler paths in release artifacts and check package contents for identifying build metadata.
- Prefer small focused modules and existing library capabilities. Do not implement speculative rule engines, recurrence, or quota policy in M1.
- Delivery retries must preserve the same OpenCode message ID. Keep uncertain admission outcomes truthful in the delivery log.
