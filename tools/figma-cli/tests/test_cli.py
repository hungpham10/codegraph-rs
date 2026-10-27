"""Tests for figma-lib CLI."""

import os
import pytest
from unittest.mock import Mock, patch

# Set dummy token for tests
os.environ["FIGMA_ACCESS_TOKEN"] = "test_token"


def test_validate_file_key():
    from figma_lib.utils import validate_file_key
    
    # Valid keys
    assert validate_file_key("AbCdEf123") == "AbCdEf123"
    assert validate_file_key("abc_def-ghi") == "abc_def-ghi"
    assert validate_file_key("  AbCd123  ") == "AbCd123"
    
    # Invalid keys
    with pytest.raises(ValueError):
        validate_file_key("")
    with pytest.raises(ValueError):
        validate_file_key("invalid key!")
    with pytest.raises(ValueError):
        validate_file_key("key@symbol")


def test_validate_node_ids():
    from figma_lib.utils import validate_node_ids
    
    # Valid
    assert validate_node_ids(["1:2", "3:4"]) == ["1:2", "3:4"]
    assert validate_node_ids(["  1:2  ", "3:4 "]) == ["1:2", "3:4"]
    
    # Invalid
    with pytest.raises(ValueError):
        validate_node_ids([])
    with pytest.raises(ValueError):
        validate_node_ids([""])


def test_cli_import():
    """Test that CLI module can be imported."""
    from figma_lib import cli
    assert cli is not None


@patch("figma_lib.figma_client.requests.Session.request")
def test_figma_client_get_file(mock_request):
    from figma_lib.figma_client import FigmaClient
    
    mock_response = Mock()
    mock_response.json.return_value = {"name": "Test File", "components": []}
    mock_response.raise_for_status.return_value = None
    mock_request.return_value = mock_response
    
    client = FigmaClient("test_token")
    result = client.get_file("AbCdEf123")
    
    assert result["name"] == "Test File"
    mock_request.assert_called_once()


@patch("figma_lib.figma_client.requests.Session.request")
def test_figma_client_get_components(mock_request):
    from figma_lib.figma_client import FigmaClient
    
    mock_response = Mock()
    mock_response.json.return_value = {"components": [{"id": "1:2", "name": "Button"}], "total": 1}
    mock_response.raise_for_status.return_value = None
    mock_request.return_value = mock_response
    
    client = FigmaClient("test_token")
    result = client.get_components("AbCdEf123")
    
    assert result["total"] == 1
    assert result["components"][0]["name"] == "Button"


@patch("figma_lib.figma_client.requests.Session.request")
def test_figma_client_get_styles(mock_request):
    from figma_lib.figma_client import FigmaClient
    
    mock_response = Mock()
    mock_response.json.return_value = {"styles": [{"id": "1:2", "name": "Primary Color"}], "total": 1}
    mock_response.raise_for_status.return_value = None
    mock_request.return_value = mock_response
    
    client = FigmaClient("test_token")
    result = client.get_styles("AbCdEf123")
    
    assert result["total"] == 1
    assert result["styles"][0]["name"] == "Primary Color"


@patch("figma_lib.figma_client.requests.Session.request")
def test_figma_client_get_images(mock_request):
    from figma_lib.figma_client import FigmaClient
    
    mock_response = Mock()
    mock_response.json.return_value = {"images": {"1:2": {"width": 100, "height": 50}}}
    mock_response.raise_for_status.return_value = None
    mock_request.return_value = mock_response
    
    client = FigmaClient("test_token")
    result = client.get_images("AbCdEf123", ["1:2"])
    
    assert "images" in result
    assert result["images"]["1:2"]["width"] == 100


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
