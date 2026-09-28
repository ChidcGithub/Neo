import tempfile
import unittest
from pathlib import Path

import numpy as np
import soundfile as sf
import torch

from robust_experiment import cpu_state_dict, event_indices, gate, make_model, normalize_audio, partition, render, seed_all, validate_features, segment, SR
from refine_candidate import development_reasons, parity
from optimize_wake import ASSETS, RUN, save_json
import json


class ControlledTrainingTests(unittest.TestCase):
    def test_stereo_resampling(self):
        t = np.arange(48000) / 48000
        a = np.stack([np.sin(2 * np.pi * 400 * t), np.zeros_like(t)], axis=1)
        out = normalize_audio(a, 48000)
        self.assertEqual(out.shape, (SR,))
        self.assertTrue(np.isfinite(out).all())
        self.assertLess(np.max(np.abs(out)), 0.51)

    def test_deterministic_full_window_noise_and_snr(self):
        speech = np.sin(np.arange(8000) * 0.1).astype(np.float32) * 0.05
        noise = np.random.default_rng(7).normal(0, 0.01, 64000).astype(np.float32)
        a = render(speech, noise, 10, np.random.default_rng(3))
        b = render(speech, noise, 10, np.random.default_rng(3))
        np.testing.assert_array_equal(a, b)
        self.assertGreater(np.std(a[:8000]), 0)
        self.assertEqual(a.shape, (32000,))
        self.assertTrue(np.isfinite(a).all())
        clean = render(speech, None, None, np.random.default_rng(3))
        active = np.abs(clean) > np.max(np.abs(clean)) * 0.04
        measured = 10 * np.log10(np.mean(clean[active] ** 2) / np.mean((a - clean) ** 2))
        self.assertAlmostEqual(measured, 10, delta=0.1)

    def test_reverb_retains_tail(self):
        speech = np.zeros(1600, np.float32)
        speech[800] = 0.1
        rir = np.zeros(3200, np.float32)
        rir[0] = 1
        rir[2500] = 0.5
        out = render(speech, None, None, np.random.default_rng(8), rir)
        self.assertEqual(np.count_nonzero(np.abs(out) > 0.001), 2)

    def test_duplicate_noise_cannot_cross_split(self):
        with tempfile.TemporaryDirectory() as folder:
            paths = []
            for i in range(20):
                p = Path(folder) / f'{i}.wav'
                sf.write(p, np.full(200, (i // 2) / 20), SR)
                paths.append(p)
            splits = partition(paths)
            for i in range(0, 20, 2):
                self.assertTrue(any(paths[i] in v and paths[i + 1] in v for v in splits.values()))
            self.assertEqual(sum(map(len, splits.values())), 20)

    def test_confirm_and_buffer_clear(self):
        self.assertEqual(event_indices([0.5, 0, 0.5]), [])
        self.assertEqual(event_indices([0.5] * 60), [1, 27, 53])
        self.assertEqual(event_indices([0.25, 0.25]), [1])

    def test_gate_rejects_lowered_operating_point(self):
        baseline = {k: {'recall': 0.9, 'hits': 90} for k in ['test_clean', 'legacy_positive', 'test_snr20', 'test_snr10', 'test_snr5', 'test_snr0', 'real_clean']}
        baseline.update({k: {'hits': 0} for k in ['test_negative', 'legacy_negative', 'legacy_background', 'acav_validation']})
        candidate = {k: dict(v) for k, v in baseline.items()}
        for k in candidate:
            if k.startswith('test_snr'):
                candidate[k]['recall'] = 1.0
        self.assertEqual(gate(baseline, candidate), [])
        candidate['test_negative']['hits'] = 1
        self.assertIn('test_negative_false_positives_increased', gate(baseline, candidate))
        candidate['test_clean']['recall'] = 0.85
        self.assertIn('clean_recall_regression', gate(baseline, candidate))

    def test_state_copy_preserves_optimizer_parameters(self):
        model = torch.nn.Linear(3, 1)
        optimizer = torch.optim.AdamW(model.parameters())
        ids = [id(p) for p in model.parameters()]
        saved = cpu_state_dict(model)
        model(torch.ones(2, 3)).sum().backward()
        optimizer.step()
        self.assertEqual(ids, [id(p) for p in optimizer.param_groups[0]['params']])
        self.assertFalse(torch.equal(saved['weight'], model.weight))
        self.assertEqual(saved['weight'].device.type, 'cpu')

    def test_empty_invalid_features_fail_clearly(self):
        for values in [np.empty((0, 16, 96)), np.zeros((1, 96)), np.full((1, 16, 96), np.nan)]:
            with self.assertRaises(ValueError):
                validate_features({'train_real': values})
        with self.assertRaises(ValueError):
            validate_features({}, ['train_real'])
        with self.assertRaises(ValueError):
            segment(np.array([]), 32000, np.random.default_rng(1))

    def test_atomic_json_replace(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'metrics.json'
            save_json(path, {'step': 1})
            save_json(path, {'step': 2})
            self.assertEqual(json.loads(path.read_text()), {'step': 2})
            self.assertFalse(path.with_suffix('.json.tmp').exists())

    def test_retention_gate_requires_all_constraints(self):
        base = {k: {'hits': 10} for k in ['dev_clean', 'dev_legacy_positive']}
        base.update({k: {'hits': 0} for k in ['dev_negative', 'dev_acav', 'dev_background']})
        base.update({f'dev_snr{s}': {'recall': .5} for s in [20, 10, 5, 0]})
        candidate = {k: dict(v) for k, v in base.items()}
        for s in [20, 10, 5, 0]:
            candidate[f'dev_snr{s}']['recall'] = .6
        self.assertEqual(development_reasons(base, candidate), [])
        candidate['dev_clean']['hits'] = 9
        self.assertIn('dev_clean_regression', development_reasons(base, candidate))

    def test_explicit_baseline_and_ort_parity(self):
        seed_all()
        with self.assertRaises(TypeError):
            make_model()
        if not (RUN / 'baseline_hi_neo.pt').exists():
            self.skipTest('本地冻结基线不存在')
        model = make_model(RUN / 'baseline_hi_neo.pt')
        if torch.cuda.is_available():
            model.cuda()
        data = np.load(RUN / 'controlled_features.npz')
        result = parity(model, ASSETS / 'hi_neo.onnx', [data[k] for k in ['dev_clean', 'dev_snr0', 'dev_negative']])
        self.assertLess(result['max_abs_error'], 1e-4)


if __name__ == '__main__':
    unittest.main()
