# sodp-mcp

MCP server that exposes [SODP](https://github.com/orkestri/SODP) state operations as AI agent tools.

## Tools

| Tool | Description |
|------|-------------|
| `sodp_read` | Read the current value of a state key |
| `sodp_watch` | Observe live updates for a key over a time window |
| `sodp_set` | Replace the full value of a state key |
| `sodp_patch` | Shallow-merge fields into an existing key |
| `sodp_set_in` | Set a single nested field via JSON Pointer |
| `sodp_delete` | Delete a state key |
| `sodp_presence` | Set an ephemeral presence entry (auto-removed on disconnect) |

## Usage

```bash
pip install sodp-mcp

# Connect to a local SODP server
SODP_URL=ws://localhost:7777 sodp-mcp

# With JWT authentication
SODP_URL=wss://myserver:7777 SODP_TOKEN=eyJ... sodp-mcp
```

## Claude Desktop configuration

```json
{
  "mcpServers": {
    "sodp": {
      "command": "sodp-mcp",
      "env": {
        "SODP_URL": "ws://localhost:7777"
      }
    }
  }
}
```

## Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `SODP_URL` | `ws://localhost:7777` | WebSocket URL of the SODP server |
| `SODP_TOKEN` | — | JWT token for authenticated servers |
