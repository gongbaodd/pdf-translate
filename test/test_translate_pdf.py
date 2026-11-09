import argparse
import importlib.util
import io
import json
import shutil
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "translate_pdf.py"
SPEC = importlib.util.spec_from_file_location("translate_pdf", MODULE_PATH)
assert SPEC and SPEC.loader
translate_pdf = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(translate_pdf)


def config_args(*, debug: bool = False, pages: str | None = None) -> argparse.Namespace:
    return argparse.Namespace(debug=debug, pages=pages)


def build_config(output_mode: str, output_dir: Path) -> str:
    return translate_pdf.build_config(
        config_args(),
        api_key="local-token",
        base_url="http://127.0.0.1:1234/v1",
        output_dir=output_dir,
        model="test/model",
        qps=1,
        output_mode=output_mode,
    )


def touch_pdf(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"%PDF-1.4\n")
    return path


class ConfigTests(unittest.TestCase):
    def test_bilingual_config_disables_mono(self) -> None:
        config = build_config("bilingual", Path("/tmp/output"))
        self.assertIn('lang-out = "zh"', config)
        self.assertIn("no-mono = true", config)
        self.assertNotIn("no-dual", config)

    def test_translated_only_config_disables_dual(self) -> None:
        config = build_config("translated-only", Path("/tmp/output"))
        self.assertIn('lang-out = "zh"', config)
        self.assertIn("no-dual = true", config)
        self.assertNotIn("no-mono", config)

    def test_bilingual_config_keeps_part_splitting_without_pages(self) -> None:
        self.assertIn("max-pages-per-part = 25", build_config("bilingual", Path("/tmp")))
        page_limited = translate_pdf.build_config(
            config_args(pages="1,2"),
            api_key="local-token",
            base_url="http://127.0.0.1:1234/v1",
            output_dir=Path("/tmp"),
            model="test/model",
            qps=1,
            output_mode="translated-only",
        )
        self.assertNotIn("max-pages-per-part", page_limited)


class ConcurrencyAndGlossaryTests(unittest.TestCase):
    def test_concurrency_sets_worker_pools(self) -> None:
        args = argparse.Namespace(debug=False, pages=None, concurrency=4, glossary=None)
        config = translate_pdf.build_config(
            args,
            api_key="local-token",
            base_url="http://127.0.0.1:1234/v1",
            output_dir=Path("/tmp"),
            model="claude-code/sonnet",
            qps=4,
        )
        self.assertIn("pool-max-workers = 4", config)
        self.assertIn("term-pool-max-workers = 4", config)
        self.assertIn("qps = 4", config)

    def test_code_font_pattern_extends_default_formula_fonts(self) -> None:
        args = argparse.Namespace(
            debug=False, pages=None, concurrency=1, glossary=None, code_font_pattern="Courier.*"
        )
        config = translate_pdf.build_config(
            args,
            api_key="local-token",
            base_url="http://127.0.0.1:1234/v1",
            output_dir=Path("/tmp"),
            model="test/model",
            qps=1,
        )
        self.assertIn("formular-font-pattern", config)
        self.assertIn("Courier.*", config)
        self.assertIn(".*Mono", config)

    def test_custom_glossary_is_used(self) -> None:
        glossary = ROOT / "glossary" / "testing-genai-en-zh.csv"
        args = argparse.Namespace(debug=False, pages=None, concurrency=1, glossary=glossary)
        config = translate_pdf.build_config(
            args,
            api_key="local-token",
            base_url="http://127.0.0.1:1234/v1",
            output_dir=Path("/tmp"),
            model="test/model",
            qps=1,
        )
        self.assertIn("testing-genai-en-zh.csv", config)
        self.assertIn("pool-max-workers = 1", config)


class FinalPathTests(unittest.TestCase):
    def test_bilingual_final_path_is_unchanged(self) -> None:
        path = translate_pdf.final_output_path(
            Path("/tmp/book.pdf"), Path("/tmp/out"), "bilingual"
        )
        self.assertEqual(path, Path("/tmp/out/book.en-zh.side-by-side.pdf"))

    def test_translated_only_final_path(self) -> None:
        path = translate_pdf.final_output_path(
            Path("/tmp/book.pdf"), Path("/tmp/out"), "translated-only"
        )
        self.assertEqual(path, Path("/tmp/out/book.zh.mono.pdf"))


class FindOutputTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tempdir = tempfile.TemporaryDirectory()
        self.output_dir = Path(self.tempdir.name)
        self.final = self.output_dir / "final.pdf"

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def test_prefers_merged_mono_over_no_watermark_variant(self) -> None:
        primary = touch_pdf(self.output_dir / "book.zh.mono.pdf")
        touch_pdf(self.output_dir / "book.no_watermark.zh.mono.pdf")
        found = translate_pdf.find_output(
            self.output_dir, set(), self.final, "translated-only"
        )
        self.assertEqual(found, primary)

    def test_accepts_single_no_watermark_mono(self) -> None:
        expected = touch_pdf(self.output_dir / "book.no_watermark.zh.mono.pdf")
        found = translate_pdf.find_output(
            self.output_dir, set(), self.final, "translated-only"
        )
        self.assertEqual(found, expected)

    def test_bilingual_ignores_mono_output(self) -> None:
        expected = touch_pdf(self.output_dir / "book.zh.dual.pdf")
        touch_pdf(self.output_dir / "book.zh.mono.pdf")
        found = translate_pdf.find_output(
            self.output_dir, set(), self.final, "bilingual"
        )
        self.assertEqual(found, expected)

    def test_ignores_decompressed_debug_artifacts(self) -> None:
        touch_pdf(self.output_dir / "book.no_watermark.zh.mono.pdf.decompressed.pdf")
        with self.assertRaises(translate_pdf.TranslationError):
            translate_pdf.find_output(
                self.output_dir, set(), self.final, "translated-only"
            )

    def test_rejects_ambiguous_mono_candidates(self) -> None:
        touch_pdf(self.output_dir / "a.zh.mono.pdf")
        touch_pdf(self.output_dir / "b.zh.mono.pdf")
        with self.assertRaises(translate_pdf.TranslationError):
            translate_pdf.find_output(
                self.output_dir, set(), self.final, "translated-only"
            )

    def test_existing_files_are_ignored(self) -> None:
        stale = touch_pdf(self.output_dir / "stale.zh.mono.pdf")
        new = touch_pdf(self.output_dir / "book.zh.mono.pdf")
        found = translate_pdf.find_output(
            self.output_dir, {stale}, self.final, "translated-only"
        )
        self.assertEqual(found, new)


class ValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        try:
            import pymupdf
        except ImportError:  # pragma: no cover
            self.skipTest("PyMuPDF is unavailable")
        self.pymupdf = pymupdf

    def write_pdf(self, width: float, height: float) -> Path:
        handle = tempfile.NamedTemporaryFile(suffix=".pdf", delete=False)
        handle.close()
        path = Path(handle.name)
        document = self.pymupdf.open()
        page = document.new_page(width=width, height=height)
        page.insert_text((20, 40), "sample text")
        document.save(path)
        document.close()
        self.addCleanup(lambda: path.unlink(missing_ok=True))
        return path

    def test_translated_only_keeps_source_geometry(self) -> None:
        path = self.write_pdf(504, 661.5)
        translate_pdf.validate_output(
            path,
            1,
            [1],
            output_mode="translated-only",
            source_size=(504, 661.5),
        )

    def test_translated_only_rejects_side_by_side_width(self) -> None:
        path = self.write_pdf(1008, 661.5)
        with self.assertRaises(translate_pdf.TranslationError):
            translate_pdf.validate_output(
                path,
                1,
                [1],
                output_mode="translated-only",
                source_size=(504, 661.5),
            )

    def test_bilingual_requires_double_width(self) -> None:
        path = self.write_pdf(1008, 661.5)
        translate_pdf.validate_output(
            path,
            1,
            [1],
            output_mode="bilingual",
            source_size=(504, 661.5),
        )

    def test_bilingual_rejects_mono_width(self) -> None:
        path = self.write_pdf(504, 661.5)
        with self.assertRaises(translate_pdf.TranslationError):
            translate_pdf.validate_output(
                path,
                1,
                [1],
                output_mode="bilingual",
                source_size=(504, 661.5),
            )


class EventTests(unittest.TestCase):
    def test_emitter_is_silent_in_text_mode(self) -> None:
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            translate_pdf.EventEmitter(False).emit("stage", stage="x")
        self.assertEqual(stdout.getvalue(), "")

    def test_emitter_writes_versioned_json_lines(self) -> None:
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            translate_pdf.EventEmitter(True).emit("stage", stage="translating")
        self.assertEqual(
            json.loads(stdout.getvalue()),
            {"v": 1, "type": "stage", "stage": "translating"},
        )

    def test_parse_progress_line(self) -> None:
        event = translate_pdf.parse_progress_line("\x1b[32mtranslate paragraphs 12/48\x1b[0m")
        self.assertEqual((event["current"], event["total"]), (12, 48))
        self.assertAlmostEqual(event["overall"], 0.25)
        percent = translate_pdf.parse_progress_line("Overall 37.5%")
        self.assertAlmostEqual(percent["overall"], 0.375)
        self.assertIsNone(translate_pdf.parse_progress_line("loading fonts"))
        self.assertIsNone(translate_pdf.parse_progress_line(""))

    def test_errors_carry_codes(self) -> None:
        with self.assertRaises(translate_pdf.TranslationError) as raised:
            translate_pdf.validate_input(Path("/nonexistent/input.pdf"))
        self.assertEqual(raised.exception.code, "input_missing")

    def test_existing_output_has_output_exists_code(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            input_pdf = touch_pdf(Path(tempdir) / "input.pdf")
            out = Path(tempdir) / "out"
            touch_pdf(out / "input.en-zh.side-by-side.pdf")
            args = argparse.Namespace(
                input=input_pdf, backend="openai-compatible", pages="1", output_dir=out,
                model="m", base_url=None, api_key_env="PI_DOCS_TEST_KEY", qps=None,
                auth_file=None, babeldoc="python3", debug=False, dry_run=False,
                output_mode="bilingual", events="json",
            )
            os_environ = translate_pdf.os.environ
            os_environ["PI_DOCS_TEST_KEY"] = "x"
            try:
                with redirect_stdout(io.StringIO()):
                    with self.assertRaises(translate_pdf.TranslationError) as raised:
                        translate_pdf.run_translation(args)
            finally:
                del os_environ["PI_DOCS_TEST_KEY"]
        self.assertEqual(raised.exception.code, "output_exists")

    def test_json_dry_run_emits_single_config_event(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            input_pdf = touch_pdf(Path(tempdir) / "input.pdf")
            args = argparse.Namespace(
                input=input_pdf, backend="openai-compatible", pages=None,
                output_dir=Path(tempdir) / "out", model="m", base_url=None,
                api_key_env="OPENAI_API_KEY", qps=None, auth_file=None,
                babeldoc="python3", debug=False, dry_run=True,
                output_mode="translated-only", events="json",
            )
            stdout = io.StringIO()
            with redirect_stdout(stdout):
                status = translate_pdf.run_translation(args)
        self.assertEqual(status, 0)
        lines = stdout.getvalue().strip().splitlines()
        self.assertEqual(len(lines), 1)
        event = json.loads(lines[0])
        self.assertEqual(event["type"], "config")
        self.assertEqual(event["v"], 1)
        self.assertTrue(event["config"]["babeldoc_no_dual"])


class DryRunTests(unittest.TestCase):
    def test_translated_only_dry_run_reports_target_without_credentials(self) -> None:
        babeldoc = ROOT / ".venv" / "bin" / "babeldoc"
        if not babeldoc.is_file() or shutil.which("node") is None:
            self.skipTest("local BabelDOC or Node.js is unavailable")

        with tempfile.TemporaryDirectory() as tempdir:
            input_pdf = touch_pdf(Path(tempdir) / "input.pdf")
            args = argparse.Namespace(
                input=input_pdf,
                backend="codex",
                pages="1",
                output_dir=Path(tempdir) / "out",
                model="test-model",
                base_url=None,
                api_key_env="OPENAI_API_KEY",
                qps=None,
                auth_file=None,
                babeldoc=str(babeldoc),
                debug=False,
                dry_run=True,
                output_mode="translated-only",
            )
            stdout = io.StringIO()
            with redirect_stdout(stdout):
                status = translate_pdf.run_translation(args)

        self.assertEqual(status, 0)
        configuration = json.loads(stdout.getvalue())
        self.assertEqual(configuration["output_mode"], "translated-only")
        self.assertTrue(configuration["babeldoc_no_dual"])
        self.assertFalse(configuration["babeldoc_no_mono"])
        self.assertTrue(
            configuration["output_file"].endswith("input.zh.mono.pdf")
        )
        serialized = json.dumps(configuration)
        self.assertNotIn("BABELDOC_BRIDGE_TOKEN", serialized)
        self.assertNotIn("api-key", serialized)

    def test_claude_code_dry_run_uses_bridge_without_auth_file(self) -> None:
        babeldoc = ROOT / ".venv" / "bin" / "babeldoc"
        if not babeldoc.is_file() or shutil.which("node") is None:
            self.skipTest("local BabelDOC or Node.js is unavailable")

        with tempfile.TemporaryDirectory() as tempdir:
            input_pdf = touch_pdf(Path(tempdir) / "input.pdf")
            args = argparse.Namespace(
                input=input_pdf,
                backend="claude-code",
                pages="1",
                output_dir=Path(tempdir) / "out",
                model="sonnet",
                base_url=None,
                api_key_env="OPENAI_API_KEY",
                qps=None,
                auth_file=None,
                babeldoc=str(babeldoc),
                claude_bin="node",
                debug=False,
                dry_run=True,
                output_mode="bilingual",
            )
            stdout = io.StringIO()
            with redirect_stdout(stdout):
                status = translate_pdf.run_translation(args)

        self.assertEqual(status, 0)
        configuration = json.loads(stdout.getvalue())
        self.assertEqual(configuration["babeldoc_model"], "claude-code/sonnet")
        self.assertEqual(configuration["qps"], 1)
        self.assertIsNone(configuration["auth_file"])
        self.assertEqual(configuration["claude_bin"], "node")
        self.assertTrue(configuration["base_url"].startswith("http://127.0.0.1:"))

    def test_claude_code_rejects_auth_file(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            input_pdf = touch_pdf(Path(tempdir) / "input.pdf")
            args = argparse.Namespace(
                input=input_pdf,
                backend="claude-code",
                pages=None,
                output_dir=Path(tempdir) / "out",
                model="sonnet",
                base_url=None,
                api_key_env="OPENAI_API_KEY",
                qps=None,
                auth_file=Path(tempdir) / "auth.json",
                babeldoc=shutil.which("true") or "true",
                claude_bin="node",
                debug=False,
                dry_run=True,
                output_mode="bilingual",
            )
            with self.assertRaisesRegex(translate_pdf.TranslationError, "auth-file"):
                translate_pdf.run_translation(args)


if __name__ == "__main__":
    unittest.main()
