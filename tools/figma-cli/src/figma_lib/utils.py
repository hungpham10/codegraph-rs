import re
import sys


class Logger:
    """Logger that prints to stderr to avoid interfering with MCP stdio transport."""

    def __init__(self, name: str = "figma-mcp"):
        self.name = name

    def _log(self, level: str, message: str) -> None:
        print(f"[{self.name}] {level}: {message}", file=sys.stderr)

    def info(self, message: str) -> None:
        self._log("INFO", message)

    def error(self, message: str) -> None:
        self._log("ERROR", message)

    def warning(self, message: str) -> None:
        self._log("WARNING", message)

    def debug(self, message: str) -> None:
        self._log("DEBUG", message)


logger = Logger()

# Figma file keys are alphanumeric strings, sometimes with hyphens/underscores
FILE_KEY_PATTERN = re.compile(r"^[a-zA-Z0-9_-]+$")


def validate_file_key(file_key: str) -> str:
    """Validate and return a Figma file key."""
    file_key = file_key.strip()
    if not file_key:
        raise ValueError("file_key must not be empty")
    if not FILE_KEY_PATTERN.match(file_key):
        raise ValueError(f"Invalid file_key format: {file_key}")
    return file_key


def validate_node_ids(node_ids: list[str]) -> list[str]:
    """Validate and return a list of Figma node IDs."""
    if not node_ids:
        raise ValueError("node_ids must not be empty")
    return [nid.strip() for nid in node_ids if nid.strip()]
