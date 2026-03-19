# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in Compas, please report it responsibly.

**Do not open a public GitHub issue for security vulnerabilities.**

Instead, please open a [private security advisory](https://github.com/ottogiron/compas/security/advisories/new) on GitHub.

You should receive a response within 48 hours. We will work with you to understand the issue and coordinate a fix before any public disclosure.

## Scope

Compas is a local CLI tool that orchestrates AI coding agents. Security-relevant areas include:

- SQLite state database access and integrity
- MCP server transport (stdio-based, local only)
- Git worktree creation and cleanup
- Backend CLI subprocess invocation
- Configuration file parsing and path resolution
