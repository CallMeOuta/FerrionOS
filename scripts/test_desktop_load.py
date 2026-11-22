import json
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from desktop_load import annotate, load_status, run_gated


class HostLoadTests(unittest.TestCase):
    def test_case_end_spike_excludes_otherwise_quiet_boot(self):
        measurements = {'host_load': [{'load_average': [13, 8, 5]}],
                        'cases': [{'frame_time_condition': True, 'strict_pointer_condition': True}]}
        status = load_status([{'load_average': [2, 3, 4]}], measurements, 12)
        annotate(measurements, status)
        self.assertEqual(status['status'], 'HOST_LIMITED')
        self.assertEqual(measurements['cases'][0]['frame_time_verdict'], 'HOST_LIMITED')

    def test_host_limited_functional_pass_is_retained_and_retried(self):
        args = SimpleNamespace(timing_budget=10, host_load_wait=10, host_load_limit=12)
        attempts = []

        def boot(args, out):
            attempts.append(out)
            load = 13 if len(attempts) == 1 else 4
            (out / 'host-load.json').write_text(json.dumps([{'load_average': [load, 3, 4]}]))
            (out / 'measurements.json').write_text(json.dumps({'cases': [
                {'frame_time_condition': False, 'strict_pointer_condition': False}]}))
            return True, 'functional pass'

        with TemporaryDirectory() as directory, patch('desktop_load.sample_host', return_value={'load_average': [1, 2, 3]}):
            out = Path(directory)
            ok, _ = run_gated(args, out, boot, RuntimeError)
            self.assertTrue(ok)
            self.assertEqual(len(attempts), 2)
            first = json.loads((attempts[0] / 'result.json').read_text())
            self.assertEqual(first['functional_verdict'], 'PASS')
            self.assertEqual(first['timing']['status'], 'HOST_LIMITED')
            selected = json.loads((out / 'measurements.json').read_text())
            self.assertEqual(selected['timing_host_gate']['status'], 'QUALIFIED')
            self.assertEqual(selected['cases'][0]['frame_time_verdict'], 'FAIL')

    def test_busy_host_expires_without_booting(self):
        args = SimpleNamespace(timing_budget=10, host_load_wait=1, host_load_limit=12)
        boot = Mock()
        with TemporaryDirectory() as directory, patch('desktop_load.sample_host', return_value={'load_average': [30, 20, 10]}), patch('desktop_load.time.monotonic', side_effect=[0, 0, 2]):
            out = Path(directory)
            with self.assertRaises(RuntimeError):
                run_gated(args, out, boot, RuntimeError)
            boot.assert_not_called()
            self.assertEqual(json.loads((out / 'timing-status.json').read_text())['status'], 'BLOCKED')


if __name__ == '__main__':
    unittest.main()
