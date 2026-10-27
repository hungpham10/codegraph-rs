# figma-cli

Python CLI for Figma REST API v1 - fetch files, components, styles, and export assets.

## Installation

```bash
pip install -e /path/to/figma-cli
# hoặc uv pip install -e /path/to/figma-cli
```

## Usage

### Set your token

```bash
export FIGMA_ACCESS_TOKEN=your_token_here
```

### Export complete file data to JSON

```bash
figma export AbCdEf123 -o my_design.json
```

### List all components in a file

```bash
figma list-components AbCdEf123
```

### List all styles in a file

```bash
figma list-styles AbCdEf123
```

### Export assets from nodes

```bash
figma export-assets AbCdEf123 1:2 3:4 -f png -s 2
```

### All commands

| Command | Description |
|---------|-------------|
| `figma export <file_key>` | Export complete file (file + components + styles) to JSON |
| `figma list-components <file_key>` | List all components in a file |
| `figma list-styles <file_key>` | List all styles in a file |
| `figma export-assets <file_key> <node_ids...>` | Export images from nodes (png, jpg, svg, pdf) |

## Library usage

```python
from figma_lib import FigmaClient

client = FigmaClient("your_token")
file_data = client.get_file("AbCdEf123")
```

## License

MIT
