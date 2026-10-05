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
RELEASE_IF = "inputs.publish"


def parse_workflow(text):
    # BaseLoader performs real YAML parsing without YAML 1.1 converting `on` to True.
    # No constructors execute arbitrary Python objects; scalar types are strings.
    class UniqueKeyLoader(yaml.BaseLoader):
        def construct_mapping(self, node, deep=False):
            seen = set()
            for key_node, _ in node.value:
                key = self.construct_object(key_node, deep=deep)
                if key in seen:
                    raise yaml.constructor.ConstructorError(
                        'while parsing workflow mapping', node.start_mark,
                        f'duplicate key: {key}', key_node.start_mark)
                seen.add(key)
            return super().construct_mapping(node, deep=deep)

    return yaml.load(text, Loader=UniqueKeyLoader)


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

    def test_release_wrapper_is_mutually_exclusive_and_least_privilege(self):
        workflow = parse_workflow((WORKFLOWS / 'release.yml').read_text(encoding='utf-8'))
        self.assertEqual(workflow['on'], {
            'push': {'tags': ['v*'], 'branches': ['main', 'release/**']},
            'pull_request': {'branches': ['main']}, 'workflow_dispatch': '',
        })
        self.assertEqual(workflow['permissions'], {'contents': 'read'})
        self.assertNotIn('env', workflow)
        self.assertEqual(workflow['concurrency'], {
            'group': 'release-${{ github.ref }}', 'cancel-in-progress': "${{ github.ref_type != 'tag' }}",
        })
        self.assertEqual(workflow['jobs'], {
            'check': {
                'if': "github.ref_type != 'tag' && github.event_name != 'workflow_dispatch'",
                'permissions': {'contents': 'read'}, 'uses': './.github/workflows/build.yml',
            },
            'release': {
                'if': "github.ref_type == 'tag' || github.event_name == 'workflow_dispatch'",
                'permissions': {'contents': 'write'}, 'uses': './.github/workflows/build.yml',
                'with': {'publish': 'true'},
            },
        })
        body = parse_workflow((WORKFLOWS / 'build.yml').read_text(encoding='utf-8'))
        self.assertEqual(list(body['jobs']), ['check'])
        self.assertNotIn('permissions', body)
        self.assertNotIn('permissions', body['jobs']['check'])
        for step in body['jobs']['check']['steps']:
            self.assertNotIn('upload-artifact', step.get('uses', ''))
            self.assertNotIn('download-artifact', step.get('uses', ''))

    def test_release_parser_install_is_pinned_and_required_before_tests(self):
        workflow = parse_workflow((WORKFLOWS / "build.yml").read_text(encoding="utf-8"))
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
        self.assertEqual(list(workflow['jobs']), ['check'])

    def test_native_preparation_precedes_every_main_cargo_command(self):
        workflow = parse_workflow((WORKFLOWS / 'build.yml').read_text(encoding='utf-8'))
        steps = workflow['jobs']['check']['steps']
        names = [step.get('name') for step in steps]
        preparations = [i for i, step in enumerate(steps) if 'tools/prepare_sherpa_ci.py' in step.get('run', '')]
        self.assertEqual(len(preparations), 1)
        index = preparations[0]
        prepare = steps[index]
        self.assertEqual(prepare['run'], 'python -B tools/prepare_sherpa_ci.py --budget-seconds 1500 --validation-seconds 180 --max-download-mib 256')
        self.assertEqual(prepare['timeout-minutes'], '30')
        self.assertNotIn('if', prepare)
        self.assertNotIn('continue-on-error', prepare)
        self.assertLess(names.index('Verify pinned drawing source and legal bodies'), index)
        clean = names.index('Invalidate cached Sherpa Rust bindings')
        self.assertLess(index, clean)
        command = 'cargo clean -p sherpa-onnx-sys -p sherpa-onnx --target x86_64-pc-windows-msvc'
        self.assertEqual(steps[clean]['shell'], 'pwsh')
        self.assertEqual(steps[clean]['run'].splitlines(), [
            command, 'if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }',
            command + ' --release', 'if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }',
        ])
        self.assertNotIn('if', steps[clean])
        cargo_steps = [i for i, step in enumerate(steps) if 'cargo ' in step.get('run', '')]
        self.assertTrue(cargo_steps)
        self.assertTrue(all(index < i and clean <= i for i in cargo_steps))
        rust_cache = next(step for step in steps if step.get('uses', '').startswith('Swatinem/rust-cache@'))
        self.assertLess(steps.index(rust_cache), index)
        self.assertEqual(workflow['env']['CARGO_TARGET_DIR'], '${{ github.workspace }}/target/ci-rust')
        self.assertEqual(rust_cache['with']['workspaces'], '. -> target/ci-rust')
        self.assertEqual(rust_cache['with']['cache-targets'], 'true')
        self.assertEqual(rust_cache['with']['cache-workspace-crates'], 'true')
        self.assertEqual(rust_cache['with']['cache-on-failure'], 'false')
        for path in ('tools/build_sherpa_asr.py', 'tools/prepare_sherpa_ci.py', 'vendor/sherpa-onnx-sys/build.rs',
                     'vendor/sherpa-onnx-sys/neo_asr.rs', 'tools/source-companions.lock.json'):
            self.assertIn(path, rust_cache['with']['key'])
        restore = next(step for step in steps if step.get('id') == 'sherpa-downloads')
        self.assertEqual(restore['with']['path'].splitlines(), ['.cache/sherpa-asr/*.tar.gz', '.cache/sherpa-asr/*.tar.bz2',
                         '.cache/sherpa-asr/*.zip', '.cache/tools/cmake-4.2.3-py3-none-win_amd64.whl'])
        for path in ('tools/build_sherpa_asr.py', 'tools/prepare_sherpa_ci.py', 'tools/source-companions.lock.json'):
            self.assertIn(path, restore['with']['key'])
        save = names.index('Save Sherpa download archives and CMake wheel')
        self.assertLess(steps.index(restore), index)
        self.assertLess(index, save)
        self.assertLess(save, clean)
        assembly = steps[names.index('Assemble package')]['run']
        self.assertIn(r'Copy-Item target\ci-rust\x86_64-pc-windows-msvc\release\neo.exe', assembly)

    def test_release_requires_pinned_drawing_source_before_build_without_policy_calls(self):
        workflow = parse_workflow((WORKFLOWS / "build.yml").read_text(encoding="utf-8"))
        steps = workflow["jobs"]["check"]["steps"]
        names = [step.get("name") for step in steps]
        source = names.index("Verify pinned drawing source and legal bodies")
        self.assertEqual(steps[source]["run"], "python tools/assemble_drawing_release.py verify-source --source target/package/drawing-src")
        checkout = names.index("Checkout pinned drawing")
        self.assertLess(names.index("Read drawing pin"), checkout)
        self.assertLess(checkout, source)
        self.assertEqual(steps[checkout]["with"]["repository"], "${{ steps.drawing-pin.outputs.repository }}")
        self.assertEqual(steps[checkout]["with"]["ref"], "${{ steps.drawing-pin.outputs.commit }}")
        self.assertEqual(steps[checkout]["with"]["path"], "target/package/drawing-src")
        for name in ("Build", "Build drawing and check both windowless versions"):
            self.assertLess(source, names.index(name))
        assembly = names.index("Assemble pinned drawing release")
        self.assertLess(names.index("Build drawing and check both windowless versions"), assembly)
        self.assertLess(assembly, names.index("Stage pinned source companions (fail closed)"))
        self.assertNotIn("if", steps[source])
        self.assertEqual(steps[assembly]["if"], RELEASE_IF)
        for index in (source, assembly):
            self.assertNotIn("continue-on-error", steps[index])
        for job in workflow["jobs"].values():
            for step in job["steps"]:
                script = step.get("run", "")
                for forbidden in ("check_distribution_review.py", "distribution-review.json", "check-approval", "verify-review"):
                    self.assertNotIn(forbidden, script)
                self.assertNotRegex(script, r"--policy[\w-]*\b")
        for step in steps:
            if RELEASE_IF in step.get('if', ''):
                self.assertNotRegex(step.get("run", ""), r"--skip[\w-]*\b")

    def test_source_delivery_is_required_before_archives_and_uses_only_publish_step_token(self):
        workflow = parse_workflow((WORKFLOWS / "build.yml").read_text(encoding="utf-8"))
        steps = workflow["jobs"]["check"]["steps"]
        names = [step.get("name") for step in steps]
        stage = steps[names.index("Stage pinned source companions (fail closed)")]
        publish = steps[names.index("Create release")]
        self.assertLess(names.index(stage["name"]), names.index("Zip portable package"))
        self.assertLess(names.index(stage["name"]), names.index("Build installer (NSIS)"))
        self.assertEqual(stage["if"], RELEASE_IF)
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
        workflow = parse_workflow((WORKFLOWS / "build.yml").read_text(encoding="utf-8"))
        steps = workflow["jobs"]["check"]["steps"]
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
        self.assertLess(prepare.index("verify_release_variants.py package"), prepare.index("stage_source_companions.py inventory"))
        self.assertIn("verify_release_variants.py archive", by_name["Zip portable package"]["run"])
        self.assertIn("verify_release_variants.py installer", installer)
        self.assertIn("--list 'v[0-9]*'", by_name["Generate changelog"]["run"])
        self.assertIn("assemble_drawing_release.py assemble", by_name["Assemble pinned drawing release"]["run"])
        self.assertTrue(any("tools.test_prepare_math_models" in s.get("run", "") for s in workflow["jobs"]["check"]["steps"]))

    def test_single_runner_builds_once_and_release_steps_are_gated(self):
        workflow = parse_workflow((WORKFLOWS / 'build.yml').read_text(encoding='utf-8'))
        self.assertEqual(list(workflow['jobs']), ['check'])
        job = workflow['jobs']['check']
        self.assertNotIn('if', job)
        self.assertNotIn('needs', job)
        self.assertNotIn('strategy', job)
        self.assertEqual(job['runs-on'], 'windows-2022')
        self.assertEqual(job['timeout-minutes'], '${{ inputs.publish && 180 || 90 }}')
        self.assertNotIn('permissions', workflow)
        self.assertNotIn('permissions', job)
        self.assertNotIn('concurrency', workflow)
        self.assertEqual(workflow['on'], {'workflow_call': {'inputs': {'publish': {
            'description': 'Package and publish after all checks pass', 'type': 'boolean', 'default': 'false',
        }}}})
        steps = job['steps']
        names = [step['name'] for step in steps]
        self.assertEqual(len(names), len(set(names)))
        ids = [step['id'] for step in steps if 'id' in step]
        self.assertEqual(len(ids), len(set(ids)))
        by_name = {step['name']: step for step in steps}
        self.assertEqual(names[:2], ['Checkout', 'Determine version'])
        self.assertEqual(by_name['Determine version']['if'], RELEASE_IF)
        self.assertEqual(by_name['Checkout']['with']['fetch-depth'], '0')
        for step in steps:
            if step.get('uses', '').startswith('actions/checkout@'):
                self.assertEqual(step['with']['persist-credentials'], 'false')
            self.assertNotIn('continue-on-error', step)
            self.assertNotIn('GH_TOKEN', workflow['env'])
            if step['name'] != 'Create release':
                self.assertNotIn('GH_TOKEN', step.get('env', {}))
                self.assertNotIn('secrets.GITHUB_TOKEN', str(step.get('env', {})))
        self.assertEqual(by_name['Create release']['env']['GH_TOKEN'], '${{ secrets.GITHUB_TOKEN }}')
        shared = [
            'Install pinned workflow YAML parser', 'Parse complete workflow YAML', 'Read drawing pin',
            'Checkout pinned drawing', 'Verify pinned drawing source and legal bodies', 'Install Rust', 'Rust cache',
            'Native preparation offline regressions', 'Source delivery offline regressions',
            'Native source binding regressions', 'ORT Eigen source notice regressions',
            'Verify pinned ORT Eigen source notices', 'SenseVoice notice regressions',
            'Verify SenseVoice license materials', 'Math model download regressions', 'Complete release variant regressions',
            'Prepare mandatory no-TTS Sherpa native', 'Invalidate cached Sherpa Rust bindings', 'Check all workspace targets',
            'Safe library tests', 'Tool policy tests', 'UIA validation and search contracts (no live desktop probe)',
            'Overlay contracts and native hit testing', 'App overlay fallback tests (no GUI or audio)',
            'Update metadata policy tests (no network)', 'Installer and release checks (isolated, never install)',
            'Drawing release root-package regressions', 'Install drawing toolchain', 'Drawing synthetic protocol tests',
            'Build drawing and check both windowless versions',
        ]
        release_build = names.index('Build')
        for name in shared:
            self.assertNotIn('if', by_name[name], name)
            self.assertLess(names.index(name), release_build, name)
        self.assertEqual(by_name['Build']['run'], 'cargo build --locked --release -p neo-app --target x86_64-pc-windows-msvc')
        for step in steps[release_build:]:
            condition = step.get('if', '')
            if step.get('uses', '').startswith('actions/cache/save@'):
                self.assertTrue(condition.startswith(RELEASE_IF + ' && '), step['name'])
                self.assertNotIn('always()', condition)
                self.assertNotIn('failure()', condition)
            else:
                self.assertEqual(condition, RELEASE_IF, step['name'])
        for command in ('tools/prepare_sherpa_ci.py ', 'assemble_drawing_release.py build ',
                        'assemble_drawing_release.py verify-source ', 'cargo build --locked --release -p neo-app ',
                        'test --locked -p board-protocol '):
            self.assertEqual(sum(step.get('run', '').count(command) for step in steps), 1, command)
        self.assertEqual(by_name['Create release'], steps[-1])

    def test_download_caches_save_validated_raw_inputs_without_build_state(self):
        workflow = parse_workflow((WORKFLOWS / 'build.yml').read_text(encoding='utf-8'))
        steps = workflow['jobs']['check']['steps']
        by_name = {step['name']: step for step in steps}
        restores = [step for step in steps if step.get('uses', '').startswith('actions/cache/restore@')]
        saves = [step for step in steps if step.get('uses', '').startswith('actions/cache/save@')]
        self.assertEqual(len(restores), len(saves))
        self.assertEqual(len(restores), 4)
        for restore in restores:
            cache_id = restore['id']
            save = next(step for step in saves if step['with']['key'] == '${{ steps.' + cache_id + '.outputs.cache-primary-key }}')
            self.assertEqual(restore['with']['path'], save['with']['path'])
            self.assertNotIn('restore-keys', restore['with'])
            self.assertIn('steps.' + cache_id + ".outputs.cache-hit != 'true'", save['if'])
            self.assertLess(steps.index(restore), steps.index(save))
            for path in restore['with']['path'].splitlines():
                self.assertTrue(path.startswith('.cache/'), path)
                for forbidden in ('target/', 'extracted', 'native/install', 'member-sha256', 'receipt'):
                    self.assertNotIn(forbidden, path)
        stt = next(step for step in restores if step['id'] == 'stt-downloads')
        self.assertEqual(stt['with']['path'].splitlines(), ['.cache/stt/sv.tar.bz2', '.cache/stt/silero_vad.onnx'])
        for digest in ('7d1efa2138a65b0b488df37f8b89e3d91a60676e416f515b952358d83dfd347e',
                       '9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6'):
            self.assertIn(digest, stt['with']['key'])
            self.assertIn(digest, by_name['Download STT models']['run'])
        mingit = next(step for step in restores if step['id'] == 'mingit-downloads')
        self.assertEqual(mingit['with']['path'], '.cache/runtime-archives')
        self.assertEqual(mingit['with']['key'], 'mingit-raw-v1-56d7b226b7693196cfc71fef26568f536c4a021ab6c37ff2db4287bed908e96e')
        self.assertIn('--archive-cache .cache/runtime-archives', by_name['Fetch MinGit runtime']['run'])
        math = next(step for step in restores if step['id'] == 'math-downloads')
        self.assertEqual(math['with']['path'], '.cache/math-models')
        self.assertEqual(math['with']['key'], "math-raw-v1-${{ hashFiles('tools/math-models.lock.json') }}")
        self.assertIn('--cache .cache/math-models', by_name['Prepare math model variants']['run'])
        for restore_name, prepare_name, save_name, consumer_name in (
            ('Restore Sherpa download archives and CMake wheel', 'Prepare mandatory no-TTS Sherpa native',
             'Save Sherpa download archives and CMake wheel', 'Check all workspace targets'),
            ('Restore STT raw downloads', 'Download STT models', 'Save STT raw downloads', 'Assemble package'),
            ('Restore math model raw downloads', 'Prepare math model variants', 'Save math model raw downloads', 'Zip portable package'),
            ('Restore MinGit raw archive', 'Fetch MinGit runtime', 'Save MinGit raw archive', 'Prepare GCM-free MinGit distribution'),
        ):
            indices = [steps.index(by_name[name]) for name in (restore_name, prepare_name, save_name, consumer_name)]
            self.assertEqual(indices, sorted(indices))

    def test_parser_rejects_duplicate_keys_instead_of_hiding_checks(self):
        for text in ('jobs:\n  check: {}\n  check: {}\n',
                     'steps:\n  - name: Verify source\n    run: verify-source\n    run: build\n'):
            with self.subTest(text=text), self.assertRaisesRegex(yaml.YAMLError, 'duplicate key'):
                parse_workflow(text)

    def test_parser_rejects_regressed_root_package_step_indentation(self):
        text = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
        old = "      - name: Drawing release root-package regressions\n"
        self.assertEqual(text.count(old), 1)
        broken = text.replace(old, "    - name: Drawing release root-package regressions\n", 1)
        with self.assertRaises(yaml.YAMLError):
            parse_workflow(broken)


if __name__ == "__main__":
    unittest.main()
