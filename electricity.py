#!/usr/bin/env python3
"""Query the captured SDU Weihai dorm electricity endpoint."""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import sqlite3
import sys
import tempfile
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path

import httpx

ROOT = Path(__file__).resolve().parent
DEFAULT_CONFIG = ROOT / ".local/electricity/request.json"
DEFAULT_HISTORY = ROOT / ".local/electricity/history.sqlite3"
ENDPOINT = "https://mcard.sdu.edu.cn/charge/feeitem/getThirdData"
FORM_FIELDS = {"feeitemid", "type", "level", "campus", "building", "floor", "room"}
ENERGY_PATTERN = re.compile(r"剩余电量为\s*(-?\d+(?:\.\d+)?)\s*度")


class QueryError(Exception):
    pass


class AuthenticationError(QueryError):
    pass


class ResponseError(QueryError):
    pass


@dataclass(frozen=True)
class Reading:
    remaining_kwh: Decimal
    supply_status: str | None


def parse_response(payload: object) -> Reading:
    if not isinstance(payload, dict):
        raise ResponseError("接口响应格式不匹配。")
    code = payload.get("code")
    if code in (401, 403, "401", "403"):
        raise AuthenticationError("登录凭据已失效，请在 App 查询一次后重新导入抓包。")
    if code != 200:
        raise ResponseError("学校接口返回业务错误，未获得有效电量。")
    mapping = payload.get("map")
    show_data = mapping.get("showData") if isinstance(mapping, dict) else None
    info = show_data.get("信息") if isinstance(show_data, dict) else None
    if not isinstance(info, str):
        raise ResponseError("响应中没有电量信息，未记录电量。")
    match = ENERGY_PATTERN.search(info)
    if not match:
        raise ResponseError("无法识别剩余电量，未记录电量。")
    supply = re.search(r"供电状态[：:]\s*(.*)", info)
    return Reading(Decimal(match.group(1)), supply.group(1).strip() if supply else None)


def validate_config(config: object) -> dict:
    if not isinstance(config, dict) or config.get("schema_version") != 1:
        raise QueryError("查询配置格式不匹配，请重新导入抓包。")
    if config.get("url") != ENDPOINT:
        raise QueryError("配置中的地址不是已验证的电费查询接口。")
    form = config.get("form")
    if not isinstance(form, dict) or set(form) != FORM_FIELDS:
        raise QueryError("配置缺少宿舍查询参数，请重新导入抓包。")
    if not all(isinstance(value, str) and value for value in form.values()):
        raise QueryError("宿舍查询参数必须是非空字符串。")
    if (form["feeitemid"], form["type"], form["level"]) != ("411", "IEC", "4"):
        raise QueryError("仅支持已验证的威海电费余额查询。")
    headers = config.get("headers")
    if not isinstance(headers, dict) or not all(
        isinstance(k, str) and isinstance(v, str) for k, v in headers.items()
    ):
        raise QueryError("认证请求头格式不匹配。")
    lowered = {key.lower(): value for key, value in headers.items()}
    if not lowered.get("synjones-auth"):
        raise QueryError("配置缺少 synjones-auth，请重新导入 App 抓包。")
    return config


def load_config(path: Path) -> dict:
    try:
        return validate_config(json.loads(path.read_text(encoding="utf-8")))
    except FileNotFoundError as exc:
        raise QueryError("找不到查询配置，请先执行 import-capture。") from exc
    except (OSError, ValueError) as exc:
        raise QueryError("无法读取查询配置，请检查文件或重新导入抓包。") from exc


def token_expiry_claim(headers: dict[str, str]) -> str | None:
    """Read metadata only; the server, not this unsigned decode, verifies login."""
    auth = next((v for k, v in headers.items() if k.lower() == "synjones-auth"), "")
    try:
        segment = auth.split()[-1].split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(segment + "=" * (-len(segment) % 4)))
        expiry = claims.get("exp")
        if not isinstance(expiry, (int, float)):
            return None
        return datetime.fromtimestamp(expiry, timezone.utc).isoformat()
    except (ValueError, IndexError, AttributeError, OverflowError, OSError):
        return None


def query(config: dict, client: httpx.Client) -> Reading:
    validate_config(config)
    try:
        response = client.post(ENDPOINT, data=config["form"], headers=config["headers"])
    except httpx.HTTPError as exc:
        raise QueryError(f"连接失败（{type(exc).__name__}），未记录电量。") from exc
    if response.status_code in (401, 403) or response.is_redirect:
        raise AuthenticationError("登录凭据已失效，请在 App 查询一次后重新导入抓包。")
    if response.status_code != 200:
        raise ResponseError(f"接口返回 HTTP {response.status_code}，未获得有效电量。")
    try:
        return parse_response(response.json())
    except ValueError as exc:
        raise ResponseError("接口未返回 JSON，可能需要重新登录或检查网络。") from exc


def import_capture(path: Path, output: Path, expected: Decimal | None = None) -> Reading:
    # Querying itself only needs httpx; mitmproxy is needed for this import command.
    from mitmproxy import http
    from mitmproxy.exceptions import FlowReadException
    from mitmproxy.io import FlowReader

    selected = None
    selected_reading = None
    try:
        with path.open("rb") as stream:
            try:
                for flow in FlowReader(stream).stream():
                    if not isinstance(flow, http.HTTPFlow) or not flow.response:
                        continue
                    request = flow.request
                    if request.method != "POST" or request.url != ENDPOINT:
                        continue
                    form = dict(request.urlencoded_form)
                    if (form.get("feeitemid"), form.get("type"), form.get("level")) != ("411", "IEC", "4"):
                        continue
                    if flow.response.status_code != 200:
                        continue
                    try:
                        reading = parse_response(flow.response.json())
                    except (QueryError, ValueError):
                        continue
                    if expected is not None and reading.remaining_kwh != expected:
                        continue
                    if selected is None or request.timestamp_start >= selected.request.timestamp_start:
                        selected, selected_reading = flow, reading
            except FlowReadException:
                # A live file may end in an unfinished entry. Only completed entries are used.
                pass
    except OSError as exc:
        raise QueryError("无法读取抓包文件。") from exc
    if selected is None or selected_reading is None:
        raise QueryError("抓包中没有符合条件的成功电费查询，请在 App 刷新余额后再导入。")
    request = selected.request
    headers = {
        k: v for k, v in request.headers.items()
        if k.lower() in {"synjones-auth", "accept", "origin", "user-agent", "x-requested-with"}
    }
    config = validate_config({
        "schema_version": 1,
        "url": ENDPOINT,
        "form": dict(request.urlencoded_form),
        "headers": headers,
        "auth_mode": "synjones-auth",
        "source_capture": str(path.resolve()),
        "captured_at": datetime.fromtimestamp(request.timestamp_start, timezone.utc).isoformat(),
        "observed_remaining_kwh": str(selected_reading.remaining_kwh),
    })
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile("w", encoding="utf-8", dir=output.parent, delete=False) as stream:
            temporary = Path(stream.name)
            json.dump(config, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
        temporary.replace(output)
        output.chmod(0o600)
    finally:
        if temporary is not None and temporary.exists():
            temporary.unlink()
    return selected_reading


def save_event(path: Path, event: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with sqlite3.connect(path) as db:
        db.execute("""CREATE TABLE IF NOT EXISTS readings (
            id INTEGER PRIMARY KEY, checked_at TEXT NOT NULL,
            remaining_kwh TEXT, supply_status TEXT, threshold_kwh TEXT,
            low_balance INTEGER, error TEXT
        )""")
        db.execute(
            "INSERT INTO readings (checked_at, remaining_kwh, supply_status, threshold_kwh, low_balance, error) "
            "VALUES (?, ?, ?, ?, ?, ?)",
            tuple(event.get(key) for key in (
                "checked_at", "remaining_kwh", "supply_status", "threshold_kwh", "low_balance", "error"
            )),
        )


def check_once(args: argparse.Namespace) -> dict:
    config = load_config(args.config)
    with httpx.Client(timeout=args.timeout, trust_env=False, follow_redirects=False) as client:
        reading = query(config, client)
    return {
        "checked_at": datetime.now(timezone.utc).isoformat(),
        "remaining_kwh": str(reading.remaining_kwh),
        "unit": "kWh",
        "supply_status": reading.supply_status,
        "threshold_kwh": str(args.threshold) if args.threshold is not None else None,
        "low_balance": reading.remaining_kwh <= args.threshold if args.threshold is not None else None,
        "token_expires_at_claim": token_expiry_claim(config["headers"]),
    }


def print_event(event: dict, as_json: bool) -> None:
    if as_json:
        print(json.dumps(event, ensure_ascii=False), flush=True)
        return
    if "error" in event:
        print(f"查询失败：{event['error']}", file=sys.stderr, flush=True)
        return
    print(f"剩余电量：{event['remaining_kwh']} 度", flush=True)
    if event["supply_status"]:
        print(f"供电状态（接口原文）：{event['supply_status']}", flush=True)
    if event["low_balance"] is True:
        print(f"低电量提醒：剩余电量已达到或低于 {event['threshold_kwh']} 度。", flush=True)


def decimal_argument(value: str) -> Decimal:
    try:
        result = Decimal(value)
    except InvalidOperation as exc:
        raise argparse.ArgumentTypeError("请填写有限数值。") from exc
    if not result.is_finite():
        raise argparse.ArgumentTypeError("请填写有限数值。")
    return result


def positive_seconds(value: str) -> float:
    result = decimal_argument(value)
    if result <= 0 or result > Decimal("31536000"):
        raise argparse.ArgumentTypeError("秒数必须大于 0，且不超过一年。")
    return float(result)


def main(argv: list[str] | None = None) -> int:
    os.umask(0o077)
    parser = argparse.ArgumentParser(description="山大威海宿舍电费查询与本地监控")
    commands = parser.add_subparsers(dest="command", required=True)
    importer = commands.add_parser("import-capture", help="从成功查询的抓包更新本地认证和宿舍参数")
    importer.add_argument("capture", type=Path)
    importer.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
    importer.add_argument("--expect", type=decimal_argument, help="只导入与页面数值一致的查询")
    for name, help_text in (("query", "独立查询一次"), ("watch", "定时查询、保存历史并在终端提醒")):
        command = commands.add_parser(name, help=help_text)
        command.add_argument("--config", type=Path, default=DEFAULT_CONFIG)
        command.add_argument("--history", type=Path, default=DEFAULT_HISTORY)
        command.add_argument("--timeout", type=positive_seconds, default=20.0)
        command.add_argument("--threshold", type=decimal_argument, default=Decimal("10") if name == "watch" else None)
        command.add_argument("--json", action="store_true")
        if name == "watch":
            command.add_argument("--interval", type=positive_seconds, default=21600.0, help="查询间隔秒数，默认 21600（6 小时）")
    args = parser.parse_args(argv)
    try:
        if args.command == "import-capture":
            reading = import_capture(args.capture, args.config, args.expect)
            print(f"已保存本地配置：{args.config}；抓包电量：{reading.remaining_kwh} 度。")
            return 0
        if args.command == "watch" and args.interval < 60:
            parser.error("监控间隔至少为 60 秒。")
        while True:
            fatal_auth = False
            try:
                event = check_once(args)
            except QueryError as exc:
                event = {"checked_at": datetime.now(timezone.utc).isoformat(), "error": str(exc)}
                fatal_auth = isinstance(exc, AuthenticationError)
            save_event(args.history, event)
            print_event(event, args.json)
            if args.command == "query" or fatal_auth:
                return 1 if "error" in event else 0
            time.sleep(args.interval)
    except QueryError as exc:
        print(str(exc), file=sys.stderr)
        return 1
    except (OSError, sqlite3.Error):
        print("本地配置或历史文件无法写入，请检查路径和权限。", file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 0


if __name__ == "__main__":
    sys.exit(main())
