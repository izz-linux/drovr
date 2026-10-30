# Security policy

## Reporting a vulnerability

Please don't open a public issue. Report it privately through GitHub instead: on the repository's **Security** tab, click **Report a vulnerability**. Only the maintainer sees the report.

Include what you found, how to reproduce it, and which version of drovr you're on (**drovr → About drovr**, or the release you downloaded). You should get a reply within a week.

## Supported versions

Only the latest release gets fixes. drovr is pre-1.0, so a fix ships in the next release rather than as a patch to older ones.

## What's in scope

drovr runs AI coding agents on your Mac with your permissions, so the places worth looking hardest are where something an agent or a web page produces could act for you:

- **The coordination MCP server** (`drovr mcp`): a tool call doing more than its description says, or reaching another agent's data.
- **Permission prompts:** a way to get a tool approved without you, or past `permissions.wait_seconds`.
- **Rendered content:** reports, Confluence previews and diffs escaping their sandbox or the content security policy.
- **Commands drovr runs:** the `[herdr]`, `jira` and `confluence` command templates, where ticket or page text could inject shell.
- **Secrets:** tokens leaking into the ledger, reports or logs past the redaction in `ledger.rs`.
- **Release builds:** anything that would let someone other than the maintainer publish a signed release.

Out of scope: what the agents themselves choose to do with the permissions you give them, and issues in Claude Code, Codex, herdr, twg or gh, which should go to those projects.
