# Baseline

This baseline records project-wide facts that active plans can reference without duplicating context. It is intentionally compact and should be refreshed from code before large refactors.

## Current Focus

- Request routing supports local Kiro credentials and external pools.
- External pools support normalized body forwarding and raw body passthrough.
- `/v1/messages`, `/na/v1/messages`, `/ha/v1/messages`, `/dfcache/*/v1/messages`, and `/cc/v1/messages` share the messages entry path.
- Body processing is already partly split by file, but capability boundaries still need to be made explicit.
