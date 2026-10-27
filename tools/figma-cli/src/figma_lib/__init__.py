"""figma-cli - Python CLI for Figma REST API v1."""

from figma_lib.config import FIGMA_ACCESS_TOKEN
from figma_lib.client import FigmaClient
from figma_lib.utils import logger, validate_file_key, validate_node_ids

__all__ = [
    "FigmaClient",
    "FIGMA_ACCESS_TOKEN",
    "logger",
    "validate_file_key",
    "validate_node_ids",
]

__version__ = "0.1.0"
