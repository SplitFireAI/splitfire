# ACP Registry submission

Everything needed to list splitfire-agent in the [ACP registry](https://agentclientprotocol.com/get-started/registry)
([agentclientprotocol/registry](https://github.com/agentclientprotocol/registry)).

## Prerequisites (done in this repo)

- `initialize` advertises a `terminal` auth method (`terminal-setup`, runs `--setup`); registry CI
  requires agent or terminal auth.
- `session/new` and `session/prompt` return `AUTH_REQUIRED` when no Onde API key is configured;
  `authenticate` re-reads the stored key without a restart.
- The release workflow builds binaries for all five registry platforms on `v*` tags.
- 16x16 monochrome `currentColor` icon at `assets/icon.svg`.

## Submitting

1. Tag and push the release: `git tag v1.0.0 && git push origin v1.0.0`. Wait for the `release`
   workflow to attach the five archives.
2. Optionally add `sha256` fields to `agent.json` for each archive (recommended, not required).
   The archive names are fixed by the workflow.
3. Fork the registry repo, then from its checkout:

   ```sh
   mkdir splitfire-agent
   cp agent.json splitfire-agent/agent.json
   cp ../assets/icon.svg splitfire-agent/icon.svg   # the icon must sit next to agent.json
   SKIP_URL_VALIDATION=1 uv run --with jsonschema .github/workflows/build_registry.py
   ```

   Drop `SKIP_URL_VALIDATION=1` once the release exists; the archive URLs are probed.
4. Open the PR. Registry CI validates the schema and icon, and probes the binary for a valid
   `authMethods` response with an empty sandbox `HOME`.
