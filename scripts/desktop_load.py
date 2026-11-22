"""Host-load records and bounded timing runs for the native desktop check."""
import json
import os
import threading
import time
import subprocess


def sample_host(phase):
    record = {'phase': phase, 'unix_ns': time.time_ns(),
              'load_average': os.getloadavg(), 'cpu_count': os.cpu_count()}
    try:
        result = subprocess.run(['uptime'], capture_output=True, text=True, timeout=5)
        record['uptime'] = result.stdout.strip()
        record['uptime_status'] = result.returncode
        if result.stderr:
            record['uptime_error'] = result.stderr.strip()
    except (OSError, subprocess.TimeoutExpired) as error:
        record['uptime_error'] = str(error)
    return record


class HostLoadMonitor:
    def __init__(self, out):
        self.out = out
        self.samples = [sample_host('boot')]
        (out / 'host-boot.json').write_text(json.dumps(self.samples[0], indent=2) + '\n')
        self.save()
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def save(self):
        (self.out / 'host-load.json').write_text(json.dumps(self.samples, indent=2) + '\n')

    def run(self):
        while not self.stop.wait(10):
            self.samples.append(sample_host('during-boot'))
            self.save()

    def close(self):
        self.stop.set()
        self.thread.join()
        self.samples.append(sample_host('end'))
        self.save()


def load_status(samples, measurements, limit):
    values = [s['load_average'][0] for s in samples]
    values.extend(s['load_average'][0] for s in measurements.get('host_load', []))
    peak = max(values) if values else None
    return {'limit': limit, 'maximum_observed_load1': peak,
            'status': 'QUALIFIED' if peak is not None and peak <= limit else 'HOST_LIMITED',
            'sampling': 'before boot, every 10 seconds, each case end, after shutdown'}


def annotate(measurements, status):
    measurements['timing_host_gate'] = status
    for case in measurements.get('cases', []):
        for condition, verdict in [('frame_time_condition', 'frame_time_verdict'),
                                   ('strict_pointer_condition', 'pointer_time_verdict')]:
            case[verdict] = ('PASS' if case[condition] else 'FAIL') if status['status'] == 'QUALIFIED' else 'HOST_LIMITED'


def run_gated(args, out, boot, blocked):
    if not hasattr(args, 'desktop_timing_deadline'):
        args.desktop_timing_deadline = time.monotonic() + args.timing_budget
    deadline = min(args.desktop_timing_deadline, time.monotonic() + args.host_load_wait)
    limit = args.host_load_limit
    record = {'requested_load1_maximum': limit, 'attempts': []}
    attempt = 0

    def save():
        (out / 'timing-status.json').write_text(json.dumps(record, indent=2) + '\n')

    while True:
        while True:
            sample = sample_host('gate-wait')
            with (out / 'gate-wait.jsonl').open('a') as stream:
                stream.write(json.dumps(sample) + '\n')
            if time.monotonic() >= deadline:
                record['status'] = 'BLOCKED'
                record['reason'] = f'host load did not allow a complete timing boot below {limit:g}'
                record['last_host_sample'] = sample
                save()
                raise blocked(record['reason'])
            if sample['load_average'][0] <= limit:
                break
            time.sleep(min(10, max(0, deadline - time.monotonic())))
        attempt += 1
        directory = out / f'attempt-{attempt}'
        directory.mkdir(parents=True, exist_ok=True)
        ok, note = boot(args, directory)
        samples = json.loads((directory / 'host-load.json').read_text())
        path = directory / 'measurements.json'
        measurements = json.loads(path.read_text()) if path.exists() else {}
        status = load_status(samples, measurements, limit)
        annotate(measurements, status)
        result = {'attempt': attempt, 'functional_verdict': 'PASS' if ok else 'FAIL',
                  'note': note, 'timing': status, 'desktop_columns': measurements}
        (directory / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        record['attempts'].append({k: v for k, v in result.items() if k != 'desktop_columns'})
        save()
        if status['status'] == 'QUALIFIED':
            record['status'] = 'QUALIFIED'
            record['selected_attempt'] = attempt
            measurements['host_load_monitor'] = samples
            (out / 'measurements.json').write_text(json.dumps(measurements, indent=2) + '\n')
            save()
            return ok, note + f'; timing host load stayed at or below {limit:g}'
        print(f'HOST_LIMITED desktop timing attempt {attempt}: peak load1 '
              f'{status["maximum_observed_load1"]:g} exceeds {limit:g}; '
              f'functional result {result["functional_verdict"]}', flush=True)
