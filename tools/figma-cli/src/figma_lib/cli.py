"""CLI commands for figma-cli - subcommands for interacting with Figma."""

import argparse
import json
import sys
from pathlib import Path

from figma_lib.client import FigmaClient
from figma_lib.config import FIGMA_ACCESS_TOKEN
from figma_lib.utils import validate_file_key, validate_node_ids


def cmd_export(args):
    """Export complete Figma file data to JSON."""
    file_key = args.file_key
    output_file = args.output if args.output else f"figma_{file_key}.json"
    output_path = Path(output_file)
    
    if not FIGMA_ACCESS_TOKEN:
        print("[ERROR] FIGMA_ACCESS_TOKEN environment variable is required", file=sys.stderr)
        sys.exit(1)
    
    # Validate file key
    validated_key = validate_file_key(file_key)
    print(f"[INFO] Validated file key: {validated_key}")
    
    client = FigmaClient(FIGMA_ACCESS_TOKEN)
    
    # Fetch file data
    print(f"[INFO] Fetching complete Figma file: {validated_key}...")
    try:
        file_data = client.get_file(validated_key)
    except Exception as e:
        print(f"[ERROR] Failed to fetch Figma file: {e}", file=sys.stderr)
        sys.exit(1)
    
    # Fetch components
    print("[INFO] Fetching components...")
    try:
        components_data = client.get_components(validated_key)
    except Exception as e:
        print(f"[WARNING] Failed to fetch components: {e}", file=sys.stderr)
        components_data = {"components": [], "total": 0}
    
    # Fetch styles
    print("[INFO] Fetching styles...")
    try:
        styles_data = client.get_styles(validated_key)
    except Exception as e:
        print(f"[WARNING] Failed to fetch styles: {e}", file=sys.stderr)
        styles_data = {"styles": [], "total": 0}
    
    # Build merged structure
    import time
    merged_data = {
        "file": {
            "key": validated_key,
            "name": file_data.get("name", "Untitled"),
            "description": file_data.get("description", ""),
            "lastModified": file_data.get("lastModified", ""),
            "document": file_data.get("document", {}),
            "components": file_data.get("components", []),
            "componentsCount": file_data.get("componentsCount", 0),
        },
        "components": components_data,
        "styles": styles_data,
        "metadata": {
            "exportedAt": time.time(),
            "exportedBy": "figma-cli",
        }
    }
    
    # Write to file
    try:
        with open(output_path, "w", encoding="utf-8") as f:
            json.dump(merged_data, f, indent=2, ensure_ascii=False)
        print(f"[SUCCESS] Data exported to: {output_path}")
        print(f"[INFO] File contains: {merged_data['file']['name']} ({merged_data['file']['componentsCount']} components)")
    except Exception as e:
        print(f"[ERROR] Failed to write output file: {e}", file=sys.stderr)
        sys.exit(1)


def cmd_list_components(args):
    """List all components in a Figma file."""
    file_key = args.file_key
    
    if not FIGMA_ACCESS_TOKEN:
        print("[ERROR] FIGMA_ACCESS_TOKEN environment variable is required", file=sys.stderr)
        sys.exit(1)
    
    validated_key = validate_file_key(file_key)
    client = FigmaClient(FIGMA_ACCESS_TOKEN)
    
    print(f"[INFO] Listing components for file: {validated_key}...")
    try:
        components_data = client.get_components(validated_key)
        components = components_data.get("components", [])
        print(f"[INFO] Found {components_data.get('total', len(components))} components")
        for comp in components:
            comp_id = comp.get("id", "unknown")
            comp_name = comp.get("name", "unnamed")
            print(f"  - {comp_name} (ID: {comp_id})")
    except Exception as e:
        print(f"[ERROR] Failed to list components: {e}", file=sys.stderr)
        sys.exit(1)


def cmd_list_styles(args):
    """List all styles in a Figma file."""
    file_key = args.file_key
    
    if not FIGMA_ACCESS_TOKEN:
        print("[ERROR] FIGMA_ACCESS_TOKEN environment variable is required", file=sys.stderr)
        sys.exit(1)
    
    validated_key = validate_file_key(file_key)
    client = FigmaClient(FIGMA_ACCESS_TOKEN)
    
    print(f"[INFO] Listing styles for file: {validated_key}...")
    try:
        styles_data = client.get_styles(validated_key)
        styles = styles_data.get("styles", [])
        print(f"[INFO] Found {styles_data.get('total', len(styles))} styles")
        for style in styles:
            style_name = style.get("name", "unnamed")
            style_id = style.get("id", "unknown")
            print(f"  - {style_name} (ID: {style_id})")
    except Exception as e:
        print(f"[ERROR] Failed to list styles: {e}", file=sys.stderr)
        sys.exit(1)


def cmd_export_assets(args):
    """Export assets (images) from Figma file nodes."""
    file_key = args.file_key
    node_ids = args.node_ids
    format_type = args.format if args.format else "png"
    scale = args.scale if args.scale else 2.0
    
    if not FIGMA_ACCESS_TOKEN:
        print("[ERROR] FIGMA_ACCESS_TOKEN environment variable is required", file=sys.stderr)
        sys.exit(1)
    
    validated_key = validate_file_key(file_key)
    validated_node_ids = validate_node_ids(node_ids)
    client = FigmaClient(FIGMA_ACCESS_TOKEN)
    
    print(f"[INFO] Exporting {format_type} assets from nodes {validated_node_ids}...")
    try:
        assets_data = client.get_images(validated_key, validated_node_ids, format=format_type, scale=scale)
        print(f"[SUCCESS] Assets exported successfully")
        # Print summary
        if "images" in assets_data:
            for key, info in assets_data["images"].items():
                if "errors" not in info:
                    print(f"  - Node {key}: {info.get('width', '?')}x{info.get('height', '?')} {format_type.upper()}")
    except Exception as e:
        print(f"[ERROR] Failed to export assets: {e}", file=sys.stderr)
        sys.exit(1)


def main():
    parser = argparse.ArgumentParser(
        prog="figma",
        description="Python CLI for Figma REST API v1 - figma-cli",
    )
    
    subparsers = parser.add_subparsers(dest="command", help="Available commands")
    
    # export command
    export_parser = subparsers.add_parser("export", help="Export complete Figma file data to JSON")
    export_parser.add_argument("file_key", help="Figma file key (e.g., AbCdEf123)")
    export_parser.add_argument("-o", "--output", help="Output JSON file path (default: figma_<key>.json)")
    
    # list-components command
    list_components_parser = subparsers.add_parser("list-components", help="List all components in a Figma file")
    list_components_parser.add_argument("file_key", help="Figma file key")
    
    # list-styles command
    list_styles_parser = subparsers.add_parser("list-styles", help="List all styles in a Figma file")
    list_styles_parser.add_argument("file_key", help="Figma file key")
    
    # export-assets command
    export_assets_parser = subparsers.add_parser("export-assets", help="Export images from Figma file nodes")
    export_assets_parser.add_argument("file_key", help="Figma file key")
    export_assets_parser.add_argument("node_ids", nargs="+", help="List of node IDs to export (e.g., 1:2 3:4)")
    export_assets_parser.add_argument("-f", "--format", choices=["png", "jpg", "svg", "pdf"], default="png", help="Export format (default: png)")
    export_assets_parser.add_argument("-s", "--scale", type=float, default=2.0, help="Scale factor (default: 2.0)")
    
    args = parser.parse_args()
    
    if not args.command:
        parser.print_help()
        sys.exit(1)
    
    # Dispatch to command handlers
    commands = {
        "export": cmd_export,
        "list-components": cmd_list_components,
        "list-styles": cmd_list_styles,
        "export-assets": cmd_export_assets,
    }
    
    commands[args.command](args)


if __name__ == "__main__":
    main()
