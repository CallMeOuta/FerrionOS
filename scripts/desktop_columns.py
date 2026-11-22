"""DD2 layout and timing check, driven through ferrion-wm's ordinary IPC."""
import json
import math
import os
import re
import shlex
import statistics
import time

from desktop_fixtures import compare_probes, load_regions

ROLES = ('files', 'editor', 'terminal')


def summary(samples):
    ordered = sorted(samples)
    return ({'p50': statistics.median(ordered),
             'p99': ordered[math.ceil(len(ordered) * .99) - 1],
             'max': ordered[-1], 'count': len(ordered)} if ordered else None)


def timings(text):
    frames = []
    for role, sequence, key, commit, presented, output in re.findall(
            r'DD2_PRESENT role=(\w+) frame=(\d+) key=(\d+) commit_ns=(\d+) presented_ns=(\d+) output_frame=(\d+)', text):
        frames.append({'role': role, 'sequence': int(sequence), 'key': int(key),
                       'commit_ns': int(commit), 'presented_ns': int(presented), 'output_frame': int(output),
                       'commit_to_present_us': (int(presented) - int(commit)) / 1000})
    inputs = [{'output_frame': int(frame), 'read_us': int(read), 'presented_us': int(done),
               'input_to_present_us': int(done) - int(read)} for frame, read, done in re.findall(
                   r'FERRION_INPUT frame=(\d+) read_us=(\d+) presented_us=(\d+)', text)]
    pointers = [{'read_us': int(read), 'completed_us': int(done), 'elapsed_us': int(done) - int(read),
                 'x': int(x), 'y': int(y)} for read, done, x, y in re.findall(
                     r'FERRION_CURSOR read_us=(\d+) completed_us=(\d+) x=(-?\d+) y=(-?\d+)', text)]
    intervals = [{'frame': int(frame), 'started_us': int(start), 'completed_us': int(done)}
                 for frame, start, done in re.findall(
                     r'FERRION_FRAME frame=(\d+) started_us=(\d+) completed_us=(\d+)', text)]
    for frame in frames:
        frame['inputs'] = [i for i in inputs if i['output_frame'] == frame['output_frame']]
    for pointer in pointers:
        pointer['completed_during_present'] = any(
            f['started_us'] <= pointer['read_us'] <= pointer['completed_us'] < f['completed_us']
            for f in intervals)
    cadence = [b['completed_us'] - a['completed_us'] for a, b in zip(intervals, intervals[1:])]
    frame_us = summary(cadence)
    key_events = [(role, int(key), int(received)) for role, key, received in re.findall(
        r'DD2_KEY role=(\w+) key=(\d+) received_ns=(\d+)', text)]
    key_updates = []
    used = set()
    for read in re.findall(r'FERRION_KEY code=30 read_ns=(\d+)', text):
        read = int(read)
        candidates = sorted((received, role, key) for role, key, received in key_events
                            if received >= read and (role, key) not in used)
        if not candidates:
            continue
        received, role, key = candidates[0]
        used.add((role, key))
        presented = [f for f in frames if f['role'] == role and f['key'] == key]
        if presented:
            first = min(presented, key=lambda f: f['presented_ns'])
            key_updates.append({'role': role, 'key': key, 'read_ns': read,
                                'received_ns': received, 'presented_ns': first['presented_ns'],
                                'output_frame': first['output_frame'],
                                'input_to_present_us': (first['presented_ns'] - read) / 1000})
    key_us = summary(k['input_to_present_us'] for k in key_updates)
    return {'frames': frames, 'inputs': inputs, 'pointers': pointers, 'present_intervals': intervals,
            'frame_us': frame_us, 'commit_to_present_us': summary(f['commit_to_present_us'] for f in frames),
            'input_to_present_us': summary(i['input_to_present_us'] for i in inputs),
            'pointer_us': summary(p['elapsed_us'] for p in pointers),
            'key_updates': key_updates, 'key_to_present_us': key_us,
            'frame_time_condition': bool(key_us and key_us['p50'] <= 16700 and key_us['p99'] <= 33000),
            'strict_pointer_condition': bool(pointers and max(p['elapsed_us'] for p in pointers) < 2000)}


def check(vm, args, qmp, boot_load, root_shell, run_status):
    result = {'width': 1280, 'height': 800, 'cases': [], 'problems': [],
              'host_cpu_count': os.cpu_count(), 'host_load': [{'phase': 'boot', 'load_average': boot_load}],
              'timing_scope': 'client commit to DRM handoff via wp_presentation CLOCK_MONOTONIC; '
                              'input read to frame handoff; input read to cursor ioctl completion; not scanout',
              'skipped_probes': [{'layout': 'tiling', 'region': 'window-files', 'point': [417, 66],
                                  'reason': 'fixture samples the rounded title corner; DD2 permits different corner treatment'}],
              'decoration_note': 'Shadows are disabled. No shadow probes occur in the three selected regions.',
              'performance_is_functional_gate': False}
    problems = result['problems']

    def save():
        (vm.out / 'measurements.json').write_text(json.dumps(result, indent=2) + '\n')

    def run(command):
        status, output = run_status(vm, 'bash -c ' + shlex.quote('source /tmp/ferrion-wm-session; ' + command), args.timeout)
        if status != 0:
            problem = f'guest command failed ({status}): {command}: {output[-1000:]}'
            problems.append(problem)
            save()
            if status is None:
                interrupted = vm.mark()
                vm.type('\x03')
                vm.wait_serial(r'bash-[\d.]+# ', 10, interrupted)
            _, diagnostics = run_status(vm, 'tail -n 100 /tmp/ferrion-wm.log; '
                                        'tail -n 12 /tmp/dd2-files.log /tmp/dd2-editor.log '
                                        '/tmp/dd2-terminal.log; ps', 20)
            (vm.out / 'command-failure.txt').write_text(diagnostics)
            vm.screenshot().save_png(vm.out / 'command-failure.png')
            raise RuntimeError(problem)
        return output

    def ipc(action):
        return run('ferrion-wm msg action ' + action)

    def windows():
        output = run('ferrion-wm msg --json windows')
        for match in re.finditer(r'\[', output):
            try:
                value, _ = json.JSONDecoder().raw_decode(output[match.start():])
                if isinstance(value, list) and all(isinstance(w, dict) and 'id' in w for w in value):
                    return value
            except ValueError:
                pass
        raise RuntimeError('window query returned no JSON array: ' + output[-1000:])

    if not root_shell(vm, args.timeout):
        problems.append('serial root shell unavailable')
        save()
        return False, problems[-1]
    shell_mark = vm.mark()
    vm.type("bash --norc --noprofile --noediting\n")
    if not vm.wait_serial(r"bash-[\d.]+# ", args.timeout, shell_mark):
        raise RuntimeError("serial Bash prompt unavailable")
    status, _ = run_status(vm, 'test -n "$BASH_VERSION"', args.timeout)
    if status != 0:
        raise RuntimeError('serial Bash did not start')
    deadline = time.monotonic() + args.timeout
    while True:
        status, _ = run_status(vm, "test -s /tmp/ferrion-wm-session", args.timeout)
        if status == 0 or time.monotonic() >= deadline:
            break
        time.sleep(.2)
    if status != 0:
        problems.append('no ferrion-wm session environment')
        vm.screenshot().save_png(vm.out / 'startup-failure.png')
        _, diagnostics = run_status(vm, 'cat /tmp/ferrion-wm.log /usr/lib/ferrion-wm-test-session /etc/ferrion-wm/config.kdl; ps', args.timeout)
        (vm.out / 'startup-diagnostics.txt').write_text(diagnostics)
        save()
        return False, problems[-1]
    for role in ROLES:
        run(f'desktop-layout-probe {role}' + (' --slow' if role == 'terminal' else '') + f' > /tmp/dd2-{role}.log 2>&1 &')
        time.sleep(.5)
    deadline = time.monotonic() + args.timeout
    current = windows()
    while len(current) < 3 and time.monotonic() < deadline:
        time.sleep(.2)
        current = windows()
    ids = {w['app_id'].removeprefix('ferrion-desktop-'): w['id'] for w in current
           if (w.get('app_id') or '').startswith('ferrion-desktop-')}
    if set(ids) != set(ROLES):
        raise RuntimeError(f'expected three probe windows, got {current}')
    for radius in (0, 14):
        ipc('load-config-file --path /etc/ferrion-wm/' + ('config.kdl' if radius == 0 else 'rounded.kdl'))
        for layout in ('tiling', 'traditional'):
            regions = load_regions(f'desktop-{layout}.svg')
            for role in ROLES:
                ident = ids[role]
                x, y, width, height = map(int, regions[f'region-window-{role}']['rect'])
                if layout == 'traditional':
                    ipc(f'move-window-to-floating --id {ident}')
                    ipc(f'set-window-width --id {ident} {width - 2}')
                    ipc(f'set-window-height --id {ident} {height - 2}')
                    ipc(f'move-floating-window --id {ident} --x {x} --y {y}')
                else:
                    ipc(f'move-window-to-tiling --id {ident}')
                    ipc(f'set-window-width --id {ident} {width - 2}')
            if layout == 'tiling':
                for index, role in enumerate(ROLES, 1):
                    ipc(f'focus-window --id {ids[role]}')
                    ipc(f'move-column-to-index {index}')
                ipc(f'focus-window --id {ids["files"]}')
            ipc(f'focus-window --id {ids["editor"]}')
            time.sleep(1)
            name = f'{layout}-corners-{radius}'
            case = {'layout': layout, 'corner_radius': radius, 'windows': windows(), 'regions': {}}
            result['cases'].append(case)
            frame = vm.screenshot()
            frame.save_png(vm.out / f'{name}.png')
            if (frame.width, frame.height) != (1280, 800):
                problems.append(f'{name}: display is {frame.width}x{frame.height}')
            for role in ROLES:
                points = regions[f'region-window-{role}']['probes']
                if layout == 'tiling' and role == 'files':
                    points = [p for p in points if p[:2] != (417, 66)]
                differences = compare_probes(frame, points)
                case['regions'][role] = {'checked': len(points), 'differences': differences}
                print(f'{"FAIL" if differences else "PASS"} {name} window-{role}: ' +
                      ('; '.join(differences) if differences else f'{len(points)} probes match'), flush=True)
                if differences:
                    problems.append(f'{name} window-{role}: ' + '; '.join(differences))
            counts = run("for log in ferrion-wm dd2-files dd2-editor dd2-terminal; do "
                         "printf 'DD2_LOG_LINES=%s:%s\\n' \"$log\" \"$(wc -l < /tmp/$log.log)\"; done")
            marks = {name: int(count) for name, count in re.findall(
                r'(?m)^DD2_LOG_LINES=([\w-]+):\s*(\d+)', counts)}
            if len(marks) != 4:
                raise RuntimeError('missing timing log boundary')
            for step in range(100):
                qmp('input-send-event', {'events': [
                    {'type': 'abs', 'data': {'axis': 'x', 'value': 1000 + step * 100}},
                    {'type': 'abs', 'data': {'axis': 'y', 'value': 1000 + step * 100}}]})
                if step % 10 == 0:
                    vm.command('sendkey a')
                time.sleep(.05)
            time.sleep(.5)
            text = run(f"tail -n +{marks['ferrion-wm'] + 1} /tmp/ferrion-wm.log")
            for role in ROLES:
                client_log = run(f"tail -n +{marks['dd2-' + role] + 1} /tmp/dd2-{role}.log")
                text += '\n'.join(line for line in client_log.splitlines() if line.startswith('DD2_')) + '\n'
            (vm.out / f"{name}-timings.log").write_text(text)
            case.update(timings(text))
            present_ids = {f['frame'] for f in case['present_intervals']}
            case['frames'] = [f for f in case['frames'] if f['output_frame'] in present_ids]
            case['commit_to_present_us'] = summary(f['commit_to_present_us'] for f in case['frames'])
            case['client_commit_to_present_us'] = {role: summary(f['commit_to_present_us'] for f in case['frames'] if f['role'] == role) for role in ROLES}
            if len(case['frames']) < 60:
                problems.append(f'{name}: only {len(case["frames"])} presented client frames')
            if len(case['pointers']) != 100:
                problems.append(f'{name}: {len(case["pointers"])} cursor timings, expected 100')
            if len(case['key_updates']) != 10:
                problems.append(f'{name}: {len(case["key_updates"])} presented key updates, expected 10')
            if not any(f['role'] == 'editor' and f['key'] > 0 for f in case['frames']):
                problems.append(f'{name}: no keyboard update presented by editor')
            if sum(f['role'] == 'terminal' for f in case['frames']) < 2:
                problems.append(f'{name}: slow client did not keep presenting')
            result['host_load'].append({'phase': name, 'load_average': os.getloadavg()})
            save()
    save()
    return not problems, '; '.join(problems) if problems else 'four layouts match; timing recorded in result.json'
