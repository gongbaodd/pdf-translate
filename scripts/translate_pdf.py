#!/usr/bin/env python3
"""Run BabelDOC with pi-ai Codex OAuth, OpenCode Go, the Claude Code CLI, or a direct OpenAI-compatible API."""

from __future__ import annotations

import argparse
import json
import os
import re
import secrets
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_INPUT = ROOT / "learningdomain-drivendesign.pdf"
DEFAULT_OUTPUT_DIR = ROOT / "output" / "pdf"
DEFAULT_GLOSSARY = ROOT / "glossary" / "ddd-en-zh.csv"
DEFAULT_AUTH_FILE = Path(
    os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))
) / "babeldoc-codex" / "auth.json"
DEFAULT_OPENCODE_GO_AUTH_FILE = Path(
    os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config"))
) / "babeldoc-opencode-go" / "auth.json"
BRIDGE_SCRIPT = ROOT / "scripts" / "translation_bridge.ts"
CODEX_ALIAS_PREFIX = "pi-codex-v1/"
OPENCODE_GO_ALIAS_PREFIX = "opencode-go/"
CLAUDE_CODE_ALIAS_PREFIX = "claude-code/"
ALIAS_PREFIXES = {
    "codex": CODEX_ALIAS_PREFIX,
    "opencode-go": OPENCODE_GO_ALIAS_PREFIX,
    "claude-code": CLAUDE_CODE_ALIAS_PREFIX,
}
BRIDGE_BACKENDS = frozenset(ALIAS_PREFIXES)
OUTPUT_MODES = ("bilingual", "translated-only")


class TranslationError(RuntimeError):
    """An actionable preflight or translation failure.

    ``code`` is a stable identifier for machine consumers (``--events json``).
    """

    def __init__(self, message: str, code: str = "error") -> None:
        super().__init__(message)
        self.code = code


EVENT_PROTOCOL_VERSION = 1


class EventEmitter:
    """Writes the line-delimited JSON event protocol used by the desktop app.

    In text mode every method is a no-op, so existing output is unchanged.
    Events never carry credentials.
    """

    def __init__(self, enabled: bool = False) -> None:
        self.enabled = enabled
        self._lock = threading.Lock()

    def emit(self, event_type: str, **fields: Any) -> None:
        if not self.enabled:
            return
        line = json.dumps(
            {"v": EVENT_PROTOCOL_VERSION, "type": event_type, **fields},
            ensure_ascii=False,
        )
        with self._lock:
            sys.stdout.write(line + "\n")
            sys.stdout.flush()

    def stage(self, stage: str) -> None:
        self.emit("stage", stage=stage)

    def warning(self, message: str) -> None:
        print(f"warning: {message}", file=sys.stderr)
        self.emit("warning", message=message)


_PROGRESS_PERCENT = re.compile(r"(\d{1,3}(?:\.\d+)?)\s*%")
_PROGRESS_COUNT = re.compile(r"(\d+)\s*/\s*(\d+)")
_ANSI = re.compile(r"\x1b\[[0-9;?]*[A-Za-z]")


def parse_progress_line(line: str) -> dict[str, Any] | None:
    """Best-effort extraction of progress from a BabelDOC console line."""
    text = _ANSI.sub("", line).strip()
    if not text:
        return None
    percent = _PROGRESS_PERCENT.search(text)
    count = _PROGRESS_COUNT.search(text)
    if percent is None and count is None:
        return None
    event: dict[str, Any] = {"stage": text[:120]}
    if count:
        event["current"], event["total"] = int(count.group(1)), int(count.group(2))
    if percent and 0 <= float(percent.group(1)) <= 100:
        event["overall"] = float(percent.group(1)) / 100
    elif count and int(count.group(2)) > 0:
        event["overall"] = min(1.0, int(count.group(1)) / int(count.group(2)))
    return event


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", nargs="?", type=Path, default=DEFAULT_INPUT)
    parser.add_argument(
        "--backend",
        choices=("codex", "opencode-go", "claude-code", "openai-compatible"),
        default="codex",
    )
    parser.add_argument("--pages", help="BabelDOC page selector, for example 31,81,151")
    parser.add_argument(
        "--output-mode",
        choices=OUTPUT_MODES,
        default="bilingual",
        help="bilingual: side-by-side PDF; translated-only: Chinese monolingual PDF",
    )
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_DIR)
    parser.add_argument("--model", required=True, help="Explicit provider model ID")
    parser.add_argument("--base-url", help="OpenAI-compatible /v1 endpoint for direct mode")
    parser.add_argument("--api-key-env", default="OPENAI_API_KEY")
    parser.add_argument("--qps", type=int)
    parser.add_argument(
        "--concurrency",
        type=int,
        default=1,
        help="Parallel bridge requests and BabelDOC workers (1-16); useful with --backend claude-code",
    )
    parser.add_argument(
        "--code-font-pattern",
        help="Regex of extra font names to keep verbatim (code listings), e.g. 'Courier.*'",
    )
    parser.add_argument(
        "--glossary",
        type=Path,
        default=DEFAULT_GLOSSARY,
        help="Glossary CSV (source,target) passed to BabelDOC",
    )
    parser.add_argument("--auth-file", type=Path)
    parser.add_argument("--babeldoc", default="babeldoc", help="BabelDOC executable")
    parser.add_argument(
        "--claude-bin",
        default=os.environ.get("CLAUDE_BIN", "claude"),
        help="Claude Code CLI executable for --backend claude-code",
    )
    parser.add_argument("--debug", action="store_true", help="Preserve BabelDOC tracebacks and debug artifacts")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument(
        "--events",
        choices=("text", "json"),
        default="text",
        help="json: write line-delimited JSON events to stdout (used by the desktop app)",
    )
    return parser.parse_args()

def auth_file_for(args: argparse.Namespace) -> Path | None:
    if args.backend == "codex":
        return (args.auth_file or DEFAULT_AUTH_FILE).expanduser().resolve()
    if args.backend == "opencode-go":
        return (args.auth_file or DEFAULT_OPENCODE_GO_AUTH_FILE).expanduser().resolve()
    return None


def check_executable(executable: str, label: str) -> None:
    if shutil.which(executable) is None:
        raise TranslationError(f"{label} executable not found: {executable}", "executable_missing")


def check_node() -> None:
    check_executable("node", "Node.js")
    completed = subprocess.run(
        ["node", "--version"], capture_output=True, text=True, check=True
    )
    version = completed.stdout.strip().lstrip("v").split(".")
    if len(version) < 2 or (int(version[0]), int(version[1])) < (22, 19):
        raise TranslationError(f"Node.js >=22.19 is required, found {completed.stdout.strip()}", "node_version")


def validate_input(path: Path) -> Path:
    path = path.expanduser().resolve()
    if not path.is_file():
        raise TranslationError(f"input PDF does not exist: {path}", "input_missing")
    if path.suffix.lower() != ".pdf":
        raise TranslationError(f"input must be a PDF: {path}", "input_invalid")
    if path.stat().st_size == 0:
        raise TranslationError(f"input PDF is empty: {path}", "input_invalid")
    return path


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


# BabelDOC 0.6.4's default "broad" formula-font pattern. Passing
# formular-font-pattern replaces it, so the default is kept and extended.
BABELDOC_FORMULA_FONT_PATTERN = (
    r"CM[^RB]|(MS|XY|MT|BL|RM|EU|LA|RS)[A-Z]|LINE|LCIRCLE|TeX-|rsfs|txsy|wasy|stmary"
    r"|.*Mono|.*Code|.*Sym|.*Math|AdvP4C4E74|AdvPSSym|AdvP4C4E59"
)


def formula_font_pattern(code_font_pattern: str) -> str:
    return f"({BABELDOC_FORMULA_FONT_PATTERN}|{code_font_pattern})"


def build_config(
    args: argparse.Namespace,
    *,
    api_key: str,
    base_url: str,
    output_dir: Path,
    model: str,
    qps: int,
    output_mode: str = "bilingual",
) -> str:
    glossary = Path(getattr(args, "glossary", None) or DEFAULT_GLOSSARY).expanduser().resolve()
    workers = getattr(args, "concurrency", 1) or 1
    lines = [
        "[babeldoc]",
        *(["debug = true"] if args.debug else []),
        'lang-in = "en"',
        'lang-out = "zh"',
        f"qps = {qps}",
        f"output = {toml_string(str(output_dir))}",
        "openai = true",
        f"openai-model = {toml_string(model)}",
        f"openai-base-url = {toml_string(base_url)}",
        f"openai-api-key = {toml_string(api_key)}",
        *(["no-dual = true"] if output_mode == "translated-only" else ["no-mono = true"]),
        'watermark-output-mode = "no_watermark"',
        *(["max-pages-per-part = 25"] if not args.pages else []),
        f"pool-max-workers = {workers}",
        f"term-pool-max-workers = {workers}",
        f"glossary-files = {toml_string(str(glossary))}",
        *(
            [f"formular-font-pattern = {toml_string(formula_font_pattern(args.code_font_pattern))}"]
            if getattr(args, "code_font_pattern", None)
            else []
        ),
        "enable-json-mode-if-requested = false",
        "report-interval = 1.0",
    ]
    return "\n".join(lines) + "\n"


def parse_explicit_pages(selector: str | None) -> list[int] | None:
    if not selector:
        return None
    pages: list[int] = []
    for item in selector.split(","):
        item = item.strip()
        if not item.isdigit() or int(item) < 1:
            return None
        pages.append(int(item))
    return sorted(set(pages))


def final_output_path(input_path: Path, output_dir: Path, output_mode: str) -> Path:
    if output_mode == "translated-only":
        return output_dir / f"{input_path.stem}.zh.mono.pdf"
    return output_dir / f"{input_path.stem}.en-zh.side-by-side.pdf"


def page_count_from_pdf(path: Path) -> int | None:
    try:
        import fitz  # type: ignore[import-not-found]
    except ImportError:
        return None
    with fitz.open(path) as document:
        return document.page_count


def page_size_from_pdf(path: Path) -> tuple[float, float] | None:
    try:
        import fitz  # type: ignore[import-not-found]
    except ImportError:
        return None
    with fitz.open(path) as document:
        if not document.page_count:
            return None
        rect = document[0].rect
        return rect.width, rect.height


def validate_output(
    path: Path,
    expected_pages: int | None,
    translation_pages: list[int] | None = None,
    *,
    output_mode: str = "bilingual",
    source_size: tuple[float, float] | None = None,
    emitter: EventEmitter | None = None,
) -> None:
    emitter = emitter or EventEmitter(False)
    if not path.is_file() or path.stat().st_size == 0:
        raise TranslationError(f"BabelDOC did not produce a non-empty output PDF: {path}", "validation_failed")
    try:
        import fitz  # type: ignore[import-not-found]
    except ImportError:
        emitter.warning("PyMuPDF unavailable; skipped PDF page and glyph validation")
        return
    with fitz.open(path) as document:
        if expected_pages is not None and document.page_count != expected_pages:
            raise TranslationError(
                f"expected {expected_pages} output pages, found {document.page_count}",
                "validation_failed",
            )
        if document.page_count and source_size is not None:
            width, height = document[0].rect.width, document[0].rect.height
            source_width, source_height = source_size
            width_multiplier = 2 if output_mode == "bilingual" else 1
            expected_width = source_width * width_multiplier
            expected_height = source_height
            if (
                abs(width - expected_width) > 12
                or abs(height - expected_height) > 12
            ):
                raise TranslationError(
                    f"unexpected {output_mode} page size: {width:.1f} x {height:.1f} points; "
                    f"expected {expected_width:.1f} x {expected_height:.1f}",
                    "validation_failed",
                )
        if translation_pages:
            sample_indexes = [
                page - 1 for page in translation_pages if 1 <= page <= document.page_count
            ]
        else:
            sample_indexes = [0, document.page_count // 2, document.page_count - 1]
        for index in sorted(set(sample_indexes)):
            text = document[index].get_text()
            if not any("\u3400" <= character <= "\u9fff" for character in text):
                emitter.warning(f"no Chinese glyph extracted from output page {index + 1}")


def wait_for_bridge(process: subprocess.Popen[str]) -> dict[str, Any]:
    if process.stdout is None:
        raise TranslationError("bridge stdout was not captured", "bridge_failed")
    line = process.stdout.readline()
    if not line:
        detail = process.poll()
        raise TranslationError(f"provider bridge exited before readiness (status={detail})", "bridge_failed")
    try:
        message = json.loads(line)
    except json.JSONDecodeError as error:
        raise TranslationError(f"provider bridge emitted invalid readiness JSON: {line!r}", "bridge_failed") from error
    if not message.get("ready") or not isinstance(message.get("port"), int):
        raise TranslationError(f"provider bridge did not become ready: {message!r}", "bridge_failed")
    return message


def check_bridge_health(port: int) -> None:
    request = urllib.request.Request(f"http://127.0.0.1:{port}/healthz", method="GET")
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            if response.status != 200:
                raise TranslationError(f"provider bridge health check returned HTTP {response.status}", "bridge_failed")
    except urllib.error.URLError as error:
        raise TranslationError(f"provider bridge health check failed: {error}", "bridge_failed") from error


def terminate(process: subprocess.Popen[str] | None) -> None:
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=10)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=10)


def find_output(
    output_dir: Path,
    before: set[Path],
    final: Path,
    output_mode: str = "bilingual",
) -> Path:
    if final.is_file():
        return final
    candidates = [
        path
        for path in output_dir.rglob("*.pdf")
        if path not in before
        and path.resolve() != final.resolve()
        and not path.name.endswith(".decompressed.pdf")
    ]
    if not candidates:
        raise TranslationError(f"no new PDF found under {output_dir}", "output_not_found")
    keywords = ("mono",) if output_mode == "translated-only" else ("dual", "bilingual")
    preferred = [
        path
        for path in candidates
        if any(word in path.name.lower() for word in keywords)
    ]
    primary = [path for path in preferred if ".no_watermark." not in path.name.lower()]
    if len(primary) == 1:
        return primary[0]
    if len(preferred) == 1:
        return preferred[0]
    if len(candidates) == 1:
        return candidates[0]
    names = ", ".join(str(path) for path in candidates)
    raise TranslationError(
        f"could not identify the {output_mode} BabelDOC output among: {names}",
        "output_not_found",
    )


def run_babeldoc(command: list[str], emitter: EventEmitter) -> int:
    """Run BabelDOC. In json mode its output goes to stderr and progress becomes events."""
    if not emitter.enabled:
        return subprocess.run(command, cwd=ROOT).returncode
    process = subprocess.Popen(
        command,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        errors="replace",
        bufsize=1,
    )
    last = -1.0
    try:
        assert process.stdout is not None
        # BabelDOC redraws progress with carriage returns, so split on both.
        for chunk in process.stdout:
            for line in re.split(r"[\r\n]+", chunk):
                if not line:
                    continue
                print(line, file=sys.stderr, flush=True)
                progress = parse_progress_line(line)
                if progress is None:
                    continue
                overall = progress.get("overall", -1.0)
                if overall == last:
                    continue
                last = overall
                emitter.emit("progress", **progress)
        return process.wait()
    finally:
        terminate(process)


def run_translation(args: argparse.Namespace, emitter: EventEmitter | None = None) -> int:
    if emitter is None:
        emitter = EventEmitter(getattr(args, "events", "text") == "json")
    input_path = validate_input(args.input)
    check_executable(args.babeldoc, "BabelDOC")
    glossary_path = Path(getattr(args, "glossary", None) or DEFAULT_GLOSSARY).expanduser().resolve()
    if not glossary_path.is_file():
        raise TranslationError(f"glossary does not exist: {glossary_path}", "glossary_missing")
    concurrency = getattr(args, "concurrency", 1) or 1
    if not 1 <= concurrency <= 16:
        raise TranslationError("--concurrency must be between 1 and 16", "invalid_option")
    args.output_dir = args.output_dir.expanduser().resolve()
    final_path = final_output_path(input_path, args.output_dir, args.output_mode)
    bridge_backend = args.backend in BRIDGE_BACKENDS
    qps = args.qps if args.qps is not None else (concurrency if bridge_backend else max(2, concurrency))
    if qps < 1:
        raise TranslationError("--qps must be at least 1", "invalid_option")
    if bridge_backend and args.base_url:
        raise TranslationError("--base-url is only valid with --backend openai-compatible", "invalid_option")
    base_url = args.base_url or "https://api.openai.com/v1"
    key_env = args.api_key_env
    api_key = os.environ.get(key_env, "")
    if args.backend == "openai-compatible" and not args.dry_run and not api_key:
        raise TranslationError(f"missing API key in environment variable {key_env}", "missing_api_key")
    auth_file = auth_file_for(args)
    claude_bin = getattr(args, "claude_bin", None) or "claude"
    if args.backend == "claude-code" and args.auth_file:
        raise TranslationError("--auth-file is not used with --backend claude-code; sign in with `claude` instead", "invalid_option")
    if bridge_backend:
        check_node()
        if not BRIDGE_SCRIPT.is_file():
            raise TranslationError(f"provider bridge does not exist: {BRIDGE_SCRIPT}", "bridge_failed")
        if args.backend == "claude-code":
            check_executable(claude_bin, "Claude Code CLI")
        model_for_babeldoc = f"{ALIAS_PREFIXES[args.backend]}{args.model}"
        base_url = "http://127.0.0.1:<ephemeral-port>/v1"
        api_key = "<per-run-local-bearer-token>"
    else:
        model_for_babeldoc = args.model

    command_preview = [
        args.babeldoc,
        "--config",
        "<temporary-restricted-config>",
        "--files",
        str(input_path),
    ]
    configuration = {
        "backend": args.backend,
        "input": str(input_path),
        "pages": args.pages,
        "model": args.model,
        "babeldoc_model": model_for_babeldoc,
        "base_url": base_url,
        "api_key_env": key_env if args.backend == "openai-compatible" else None,
        "api_key_configured": bool(os.environ.get(key_env)) if args.backend == "openai-compatible" else None,
        "qps": qps,
        "concurrency": concurrency,
        "glossary": str(glossary_path),
        "code_font_pattern": getattr(args, "code_font_pattern", None),
        "output_mode": args.output_mode,
        "babeldoc_no_mono": args.output_mode == "bilingual",
        "babeldoc_no_dual": args.output_mode == "translated-only",
        "output_dir": str(args.output_dir),
        "output_file": str(final_path),
        "auth_file": str(auth_file) if auth_file else None,
        "claude_bin": claude_bin if args.backend == "claude-code" else None,
        "command": command_preview,
    }
    if args.dry_run:
        if emitter.enabled:
            emitter.emit("config", config=configuration)
        else:
            print(json.dumps(configuration, indent=2, ensure_ascii=False))
        return 0
    emitter.emit("config", config=configuration)

    if final_path.exists():
        raise TranslationError(f"refusing to overwrite existing output: {final_path}", "output_exists")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    before = set(args.output_dir.rglob("*.pdf"))
    bridge: subprocess.Popen[str] | None = None
    token = secrets.token_urlsafe(48)
    temporary_directory = tempfile.TemporaryDirectory(prefix="babeldoc-translation-")
    config_path = Path(temporary_directory.name) / "babeldoc.toml"
    try:
        if bridge_backend:
            emitter.stage("bridge_starting")
            environment = os.environ.copy()
            environment["BABELDOC_BRIDGE_TOKEN"] = token
            bridge = subprocess.Popen(
                [
                    "node",
                    str(BRIDGE_SCRIPT),
                    "--port",
                    "0",
                    "--provider",
                    args.backend,
                    "--model",
                    args.model,
                    "--concurrency",
                    str(concurrency),
                    *(
                        ["--claude-bin", claude_bin]
                        if args.backend == "claude-code"
                        else ["--auth-file", str(auth_file)]
                    ),
                ],
                cwd=ROOT,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=None,
                text=True,
                start_new_session=True,
            )
            ready = wait_for_bridge(bridge)
            port = ready["port"]
            check_bridge_health(port)
            emitter.stage("bridge_ready")
            base_url = f"http://127.0.0.1:{port}/v1"
            api_key = token
        config_path.write_text(
            build_config(
                args,
                api_key=api_key,
                base_url=base_url,
                output_dir=args.output_dir,
                model=model_for_babeldoc,
                qps=qps,
                output_mode=args.output_mode,
            ),
            encoding="utf-8",
        )
        os.chmod(config_path, 0o600)
        command = [args.babeldoc, "--config", str(config_path), "--files", str(input_path)]
        if args.pages:
            command.extend(["--pages", args.pages])
        print("Running:", " ".join(command), file=sys.stderr if emitter.enabled else sys.stdout, flush=True)
        emitter.stage("translating")
        returncode = run_babeldoc(command, emitter)
        if returncode != 0:
            raise TranslationError(f"BabelDOC failed with exit status {returncode}", "babeldoc_failed")
        emitter.stage("validating")
        source_output = find_output(
            args.output_dir, before, final_path, args.output_mode
        )
        explicit_pages = parse_explicit_pages(args.pages)
        expected_pages = page_count_from_pdf(input_path)
        source_size = page_size_from_pdf(input_path)
        validate_output(
            source_output,
            expected_pages,
            explicit_pages,
            output_mode=args.output_mode,
            source_size=source_size,
            emitter=emitter,
        )
        if source_output.resolve() != final_path.resolve():
            if final_path.exists():
                raise TranslationError(f"refusing to overwrite existing output: {final_path}", "output_exists")
            source_output.rename(final_path)
        if emitter.enabled:
            emitter.emit("result", output_file=str(final_path), pages=expected_pages)
        else:
            print(f"Wrote {final_path}")
        return 0
    finally:
        terminate(bridge)
        temporary_directory.cleanup()


def _raise_interrupt(signum: int, _frame: Any) -> None:
    # Let `finally` blocks stop the bridge and delete the temporary config.
    raise KeyboardInterrupt


def main() -> int:
    args = parse_args()
    emitter = EventEmitter(args.events == "json")
    signal.signal(signal.SIGTERM, _raise_interrupt)
    if hasattr(signal, "SIGHUP"):
        signal.signal(signal.SIGHUP, _raise_interrupt)
    try:
        return run_translation(args, emitter)
    except (TranslationError, OSError, ValueError) as error:
        print(f"translate_pdf: {error}", file=sys.stderr)
        emitter.emit("error", code=getattr(error, "code", "error"), message=str(error))
        return 2
    except KeyboardInterrupt:
        emitter.emit("error", code="cancelled", message="cancelled")
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
