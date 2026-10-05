"""Parse complete workflow YAML offline; PyYAML is a development-test dependency."""
from pathlib import Path
import unittest

try:
    import yaml
except ModuleNotFoundError as error:
    if error.name != "yaml":
        raise
    yaml = None


WORKFLOWS = Path(__file__).resolve().parents[1] / ".github/workflows"


def parse_workflow(text):
    # BaseLoader performs real YAML parsing without YAML 1.1 converting `on` to True.
    # No constructors execute arbitrary Python objects; scalar types are strings.
    return yaml.load(text, Loader=yaml.BaseLoader)


@unittest.skipIf(yaml is None, "Development test needs PyYAML==6.0.3; CI installs and checks it explicitly")
class WorkflowSyntaxTests(unittest.TestCase):
    def test_all_workflows_parse_as_job_step_mappings(self):
        paths = sorted([*WORKFLOWS.glob("*.yml"), *WORKFLOWS.glob("*.yaml")])
        self.assertTrue(paths, "No workflow files found")
        for path in paths:
            with self.subTest(workflow=path.name):
                workflow = parse_workflow(path.read_text(encoding="utf-8"))
                self.assertIsInstance(workflow, dict)
                self.assertIn("on", workflow)
                self.assertIsInstance(workflow["jobs"], dict)
                self.assertTrue(workflow["jobs"])
                for name, job in workflow["jobs"].items():
                    self.assertIsInstance(job, dict, name)
                    if "uses" in job:  # reusable workflow job
                        continue
                    self.assertIsInstance(job.get("steps"), list, name)
                    self.assertTrue(job["steps"], name)
                    for step in job["steps"]:
                        self.assertIsInstance(step, dict, name)
                        self.assertNotIn("steps", step)
                        self.assertNotIn("jobs", step)
                        self.assertNotEqual("run" in step, "uses" in step, step)
                        self.assertIsInstance(step.get("run", step.get("uses")), str)

    def test_release_parser_install_is_pinned_and_required_before_tests(self):
        workflow = parse_workflow((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        steps = workflow["jobs"]["check"]["steps"]
        install = next(i for i, step in enumerate(steps) if step.get("name") == "Install pinned workflow YAML parser")
        test = next(i for i, step in enumerate(steps) if step.get("name") == "Parse complete workflow YAML")
        self.assertEqual(steps[install]["run"], "python -m pip install PyYAML==6.0.3")
        self.assertLess(install, test)
        self.assertLess(test, next(i for i, step in enumerate(steps) if step.get("name") == "Install Rust"))
        for step in (steps[install], steps[test]):
            self.assertNotIn("if", step)
            self.assertNotIn("continue-on-error", step)
        script = steps[test]["run"]
        self.assertIn("import yaml; assert yaml.__version__ == '6.0.3'", script)
        self.assertIn("python -B -m unittest tools.test_workflow_syntax -v", script)
        self.assertEqual(script.count("if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }"), 2)
        root_tests = next(step for step in steps if step.get("name") == "Drawing release root-package regressions")
        self.assertIn("tools.test_release_workflow", root_tests["run"])
        self.assertIn("tools.test_assemble_drawing_release", root_tests["run"])
        self.assertNotIn("env", root_tests)
        self.assertEqual(workflow["jobs"]["release"]["needs"], ["check", "drawing-check"])

    def test_native_preparation_precedes_every_main_cargo_command(self):
        workflow = parse_workflow((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        for job_name in ("check", "release"):
            with self.subTest(job=job_name):
                steps = workflow["jobs"][job_name]["steps"]
                preparations = [i for i, step in enumerate(steps)
                                if "tools/prepare_sherpa_ci.py" in step.get("run", "")]
                self.assertEqual(len(preparations), 1)
                index = preparations[0]
                prepare = steps[index]
                self.assertEqual(prepare["run"], "python -B tools/prepare_sherpa_ci.py --budget-seconds 600 --validation-seconds 180 --max-download-mib 256")
                self.assertEqual(prepare["timeout-minutes"], "30")
                self.assertNotIn("if", prepare)
                self.assertNotIn("continue-on-error", prepare)
                cargo_steps = [i for i, step in enumerate(steps)
                               if "cargo " in step.get("run", "")]
                self.assertTrue(cargo_steps)
                self.assertTrue(all(index < i for i in cargo_steps))
                rust_cache = next(step for step in steps if step.get("uses", "").startswith("Swatinem/rust-cache@"))
                self.assertEqual(rust_cache["with"]["cache-targets"], "false")
                caches = [step for step in steps if step.get("uses", "").startswith("actions/cache@")]
                self.assertEqual(len(caches), 1)
                paths = caches[0]["with"]["path"].splitlines()
                self.assertEqual(paths, [".cache/sherpa-asr/*.tar.gz", ".cache/sherpa-asr/*.tar.bz2",
                                         ".cache/sherpa-asr/*.zip", ".cache/tools/cmake-4.2.3-py3-none-win_amd64.whl"])
                key = caches[0]["with"]["key"]
                self.assertIn("tools/build_sherpa_asr.py", key)
                self.assertIn("tools/prepare_sherpa_ci.py", key)
                self.assertNotIn("restore-keys", caches[0]["with"])
                if job_name == "release":
                    source = next(i for i, step in enumerate(steps)
                                  if step.get("name") == "Verify pinned drawing source and legal bodies")
                    self.assertLess(source, index)
        drawing = workflow["jobs"]["drawing-check"]["steps"]
        self.assertFalse(any("prepare_sherpa_ci" in step.get("run", "") for step in drawing))

    def test_release_requires_pinned_drawing_source_before_build_without_policy_calls(self):
        workflow = parse_workflow((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        steps = workflow["jobs"]["release"]["steps"]
        names = [step.get("name") for step in steps]
        source = names.index("Verify pinned drawing source and legal bodies")
        self.assertEqual(steps[source]["run"], "python tools/assemble_drawing_release.py verify-source --source target/package/drawing-src")
        checkout = names.index("Checkout pinned drawing for release")
        self.assertLess(names.index("Read drawing release pin"), checkout)
        self.assertLess(checkout, source)
        self.assertEqual(steps[checkout]["with"]["repository"], "${{ steps.drawing-pin.outputs.repository }}")
        self.assertEqual(steps[checkout]["with"]["ref"], "${{ steps.drawing-pin.outputs.commit }}")
        self.assertEqual(steps[checkout]["with"]["path"], "target/package/drawing-src")
        for name in ("Build", "Build drawing release and check both windowless versions"):
            self.assertLess(source, names.index(name))
        assembly = names.index("Assemble pinned drawing release")
        self.assertLess(names.index("Build drawing release and check both windowless versions"), assembly)
        self.assertLess(assembly, names.index("Stage pinned source companions (fail closed)"))
        for index in (source, assembly):
            self.assertNotIn("if", steps[index])
            self.assertNotIn("continue-on-error", steps[index])
        for job in workflow["jobs"].values():
            for step in job["steps"]:
                script = step.get("run", "")
                for forbidden in ("check_distribution_review.py", "distribution-review.json", "check-approval", "verify-review"):
                    self.assertNotIn(forbidden, script)
                self.assertNotRegex(script, r"--policy[\w-]*\b")
        for step in steps:
            self.assertNotRegex(step.get("run", ""), r"--skip[\w-]*\b")

    def test_source_delivery_is_required_before_archives_and_uses_only_publish_step_token(self):
        workflow = parse_workflow((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        steps = workflow["jobs"]["release"]["steps"]
        names = [step.get("name") for step in steps]
        stage = steps[names.index("Stage pinned source companions (fail closed)")]
        publish = steps[names.index("Create release")]
        self.assertLess(names.index(stage["name"]), names.index("Zip portable package"))
        self.assertLess(names.index(stage["name"]), names.index("Build installer (NSIS)"))
        self.assertNotIn("if", stage)
        self.assertNotIn("continue-on-error", stage)
        self.assertNotIn("continue-on-error", publish)
        self.assertEqual(stage["timeout-minutes"], "30")
        self.assertNotIn("GH_TOKEN", stage["env"])
        self.assertEqual(publish["env"]["GH_TOKEN"], "${{ secrets.GITHUB_TOKEN }}")
        for step in (stage, publish):
            self.assertEqual(step["env"]["RELEASE_REPOSITORY"], "${{ github.repository }}")
            self.assertEqual(step["env"]["RELEASE_TAG"], "${{ steps.ver.outputs.tag }}")
        check = workflow["jobs"]["check"]["steps"]
        tests = next(step for step in check if step.get("name") == "Source delivery offline regressions")
        self.assertEqual(tests["run"], "python -B -m unittest tools.test_stage_source_companions -v")
        self.assertNotIn("if", tests)

    def test_two_math_variants_share_base_and_publish_together(self):
        workflow = parse_workflow((WORKFLOWS / "release.yml").read_text(encoding="utf-8"))
        steps = workflow["jobs"]["release"]["steps"]
        by_name = {s.get("name"): s for s in steps}
        names = list(by_name)
        self.assertLess(names.index("Stage pinned source companions (fail closed)"), names.index("Prepare math model variants"))
        self.assertLess(names.index("Prepare math model variants"), names.index("Zip portable package"))
        prepare = by_name["Prepare math model variants"]["run"]
        self.assertIn("@('int8', 'fp32')", prepare)
        self.assertIn("Copy-Item -Recurse dist/neo $pkg", prepare)
        self.assertIn("tools/prepare_math_models.py --variant $variant --package $pkg", prepare)
        self.assertNotIn("quantize", prepare)
        for name in ("Prepare math model variants", "Zip portable package", "Build installer (NSIS)"):
            script = by_name[name]["run"]
            self.assertIn("@('int8', 'fp32')", script)
            self.assertIn("$LASTEXITCODE", script)
            self.assertNotIn("continue-on-error", by_name[name])
        self.assertIn("-$variant-portable-x64.zip", by_name["Zip portable package"]["run"])
        installer = by_name["Build installer (NSIS)"]["run"]
        self.assertIn(r"/DPACKAGE_DIR=dist\neo-$variant", installer)
        self.assertIn("/DOUTPUT_FILE=$out", installer)
        self.assertIn("-$variant-installer-x64.exe", installer)
        self.assertIn("--variants int8 fp32", by_name["Create release"]["run"])
        self.assertIn("--list 'v[0-9]*'", by_name["Generate changelog"]["run"])
        self.assertIn("assemble_drawing_release.py assemble", by_name["Assemble pinned drawing release"]["run"])
        self.assertTrue(any("tools.test_prepare_math_models" in s.get("run", "") for s in workflow["jobs"]["check"]["steps"]))

    def test_parser_rejects_regressed_root_package_step_indentation(self):
        text = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
        old = "      - name: Drawing release root-package regressions\n"
        self.assertEqual(text.count(old), 1)
        broken = text.replace(old, "    - name: Drawing release root-package regressions\n", 1)
        with self.assertRaises(yaml.YAMLError):
            parse_workflow(broken)


if __name__ == "__main__":
    unittest.main()
