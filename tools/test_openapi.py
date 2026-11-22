from __future__ import annotations

import re
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
SPEC = yaml.safe_load((ROOT / "docs" / "openapi.yaml").read_text(encoding="utf-8"))
HANDLERS = (ROOT / "src" / "web" / "handlers.rs").read_text(encoding="utf-8")
ROUTER = (ROOT / "src" / "web" / "mod.rs").read_text(encoding="utf-8")

REQUIRED_PARAMS = {
    "q",
    "format",
    "engines",
    "pageno",
    "timeout_limit",
    "strict_engines",
    "max_results",
    "include_raw_content",
}


def documented_params(path: str, method: str = "get") -> dict:
    operation = SPEC["paths"][path][method]
    found = {}
    for parameter in operation.get("parameters", []):
        if "$ref" in parameter:
            name = parameter["$ref"].rsplit("/", 1)[-1]
            found[SPEC["components"]["parameters"][name]["name"]] = parameter
        else:
            found[parameter["name"]] = parameter
    return found


class OpenApiContractTests(unittest.TestCase):
    def test_spec_parses_and_covers_the_json_api(self) -> None:
        self.assertTrue(SPEC["openapi"].startswith("3."))
        for path in ("/search", "/autocompleter", "/research", "/healthz", "/readyz"):
            self.assertIn(path, SPEC["paths"])

    def test_documented_search_parameters_exist_in_the_handler(self) -> None:
        struct = re.search(r"pub struct SearchParams \{(.*?)\n\}", HANDLERS, re.S)
        self.assertIsNotNone(struct)
        fields = set(re.findall(r"pub ([a-z_]+):", struct.group(1)))
        documented = documented_params("/search")
        self.assertTrue(REQUIRED_PARAMS <= set(documented), REQUIRED_PARAMS - set(documented))
        for name in documented:
            self.assertIn(name, fields, f"documented parameter {name} is not a SearchParams field")

    def test_limits_in_the_spec_match_the_code(self) -> None:
        settings_source = (ROOT / "src" / "settings.rs").read_text(encoding="utf-8")
        page_limit = re.search(r"pub const MAX_PAGENO: u32 = (\d+);", settings_source)
        self.assertIsNotNone(page_limit)
        self.assertEqual(
            documented_params("/search")["pageno"]["schema"]["maximum"], int(page_limit.group(1))
        )

        query_limit = re.search(r"const MAX_QUERY_CHARS: usize = (\d+);", HANDLERS)
        self.assertIsNotNone(query_limit)
        self.assertEqual(
            SPEC["components"]["parameters"]["Q"]["schema"]["maxLength"], int(query_limit.group(1))
        )

        research = re.search(
            r"max_queries\s*\.unwrap_or\(settings\.research\.max_queries\)\s*\.clamp\(1, (\d+)\)",
            HANDLERS,
            re.S,
        )
        self.assertIsNotNone(research)
        max_queries = documented_params("/research")["max_queries"]["schema"]["maximum"]
        self.assertEqual(max_queries, int(research.group(1)))

    def test_error_bodies_are_json_on_the_json_endpoints(self) -> None:
        for path, statuses in (("/search", ("400", "429")), ("/research", ("400", "429", "504"))):
            for status in statuses:
                body = SPEC["paths"][path]["get"]["responses"][status]["content"]["application/json"]["schema"]
                self.assertEqual(body, {"$ref": "#/components/schemas/Error"}, f"{path} {status}")

    def test_documented_paths_are_routes_of_the_router(self) -> None:
        routes = set(re.findall(r'\.route\(\s*"(/[^"]*)"', ROUTER))
        for path in SPEC["paths"]:
            self.assertIn(path, routes, f"{path} is documented but not routed")


if __name__ == "__main__":
    unittest.main()