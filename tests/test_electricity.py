import json
import sqlite3
import tempfile
import unittest
from decimal import Decimal
from pathlib import Path

import httpx

import electricity


def config():
    return {
        "schema_version": 1,
        "url": electricity.ENDPOINT,
        "form": {
            "feeitemid": "411", "type": "IEC", "level": "4", "campus": "example",
            "building": "example", "floor": "example", "room": "example",
        },
        "headers": {"synjones-auth": "bearer test-token"},
    }


def payload(info):
    return {"code": 200, "map": {"showData": {"信息": info}}}


class ElectricityTests(unittest.TestCase):
    def test_power_status_failure_keeps_valid_energy(self):
        reading = electricity.parse_response(payload("剩余电量为35.67度，供电状态：查询失败   "))
        self.assertEqual(reading.remaining_kwh, Decimal("35.67"))
        self.assertEqual(reading.supply_status, "查询失败")

    def test_zero_and_negative_energy_are_valid(self):
        for value in ("0", "-1.23"):
            with self.subTest(value=value):
                self.assertEqual(
                    electricity.parse_response(payload(f"剩余电量为{value}度")).remaining_kwh,
                    Decimal(value),
                )

    def test_missing_energy_is_an_error(self):
        with self.assertRaises(electricity.ResponseError):
            electricity.parse_response(payload("剩余电量查询失败"))

    def test_other_charge_actions_are_rejected_before_network(self):
        unsafe = config()
        unsafe["form"]["type"] = "pay"
        with self.assertRaises(electricity.QueryError):
            electricity.validate_config(unsafe)

    def test_authentication_failure_is_reported(self):
        with httpx.Client(transport=httpx.MockTransport(lambda _: httpx.Response(401))) as client:
            with self.assertRaises(electricity.AuthenticationError):
                electricity.query(config(), client)

    def test_login_redirect_is_not_followed(self):
        requests = []

        def handler(request):
            requests.append(request)
            return httpx.Response(302, headers={"location": "https://example.invalid/login"})

        with httpx.Client(transport=httpx.MockTransport(handler), follow_redirects=False) as client:
            with self.assertRaises(electricity.AuthenticationError):
                electricity.query(config(), client)
        self.assertEqual(len(requests), 1)

    def test_failed_query_has_null_energy_in_history(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "history.sqlite3"
            electricity.save_event(path, {"checked_at": "now", "error": "network failure"})
            with sqlite3.connect(path) as db:
                energy, error = db.execute("SELECT remaining_kwh, error FROM readings").fetchone()
            self.assertIsNone(energy)
            self.assertEqual(error, "network failure")

    def test_import_only_successful_readonly_query(self):
        from mitmproxy import http
        from mitmproxy.io import FlowWriter
        from mitmproxy.test.tflow import tflow

        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory) / "test.mitm"
            output = Path(directory) / "request.json"
            with capture.open("wb") as stream:
                writer = FlowWriter(stream)
                for action, status in (("pay", 200), ("IEC", 401), ("IEC", 200)):
                    flow = tflow()
                    flow.request = http.Request.make("POST", electricity.ENDPOINT)
                    flow.request.urlencoded_form = {**config()["form"], "type": action}
                    flow.request.headers["synjones-auth"] = "bearer test-token"
                    flow.request.headers["cookie"] = "unneeded=test"
                    flow.response = http.Response.make(status, json.dumps(payload("剩余电量为35.67度")).encode())
                    writer.add(flow)
            reading = electricity.import_capture(capture, output, Decimal("35.67"))
            self.assertEqual(reading.remaining_kwh, Decimal("35.67"))
            imported = electricity.load_config(output)
            self.assertNotIn("cookie", {k.lower(): v for k, v in imported["headers"].items()})
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
