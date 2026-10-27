import os

from dotenv import load_dotenv

load_dotenv()

FIGMA_ACCESS_TOKEN = os.getenv("FIGMA_ACCESS_TOKEN")

if not FIGMA_ACCESS_TOKEN:
    raise ValueError(
        "FIGMA_ACCESS_TOKEN environment variable is required. "
        "Set it in your .env file or export it in your shell."
    )
