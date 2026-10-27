import requests

from figma_lib.utils import logger

BASE_URL = "https://api.figma.com/v1"


class FigmaClient:
    """Client for the Figma REST API."""

    def __init__(self, access_token: str):
        self.session = requests.Session()
        self.session.headers.update({
            "X-Figma-Token": access_token,
        })
        logger.info("FigmaClient initialized")

    def _request(self, method: str, path: str, **kwargs) -> dict:
        url = f"{BASE_URL}{path}"
        logger.debug(f"{method} {url}")
        try:
            response = self.session.request(method, url, **kwargs)
            response.raise_for_status()
            return response.json()
        except requests.exceptions.HTTPError as e:
            logger.error(f"HTTP error {e.response.status_code}: {e.response.text}")
            raise
        except requests.exceptions.RequestException as e:
            logger.error(f"Request failed: {e}")
            raise

    def get_file(self, file_key: str) -> dict:
        """Get a Figma file by its key."""
        return self._request("GET", f"/files/{file_key}")

    def get_nodes(self, file_key: str, node_ids: list[str]) -> dict:
        """Get specific nodes from a Figma file."""
        ids = ",".join(node_ids)
        return self._request("GET", f"/files/{file_key}/nodes", params={"ids": ids})

    def get_components(self, file_key: str) -> dict:
        """Get all components in a Figma file."""
        return self._request("GET", f"/files/{file_key}/components")

    def get_styles(self, file_key: str) -> dict:
        """Get all styles in a Figma file."""
        return self._request("GET", f"/files/{file_key}/styles")

    def get_images(
        self,
        file_key: str,
        node_ids: list[str],
        format: str = "png",
        scale: float = 2,
    ) -> dict:
        """Export images for the given nodes."""
        ids = ",".join(node_ids)
        return self._request(
            "GET",
            f"/images/{file_key}",
            params={"ids": ids, "format": format, "scale": scale},
        )
