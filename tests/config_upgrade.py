"""Verify existing optional settings survive a real, offline container recreation.

Run: python3 tests/config_upgrade.py IMAGE
Uses only a unique disposable volume; never opens a WhatsApp connection.
"""
import hashlib
import json
import subprocess
import sys
import time
import uuid

image = sys.argv[1]
name = 'carbon-config-' + uuid.uuid4().hex[:12]
volume = name + '-data'
config = json.dumps({
    'listen': '127.0.0.1:18080',
    'auth': {'mode': 'disabled'},
    'experimental_tasks': True,
    'debug_trace': True,
    'model_concurrency': 3,
}, indent=2) + '\n'
digest = hashlib.sha256(config.encode()).hexdigest()


def docker(*args, input=None):
    return subprocess.check_output(['docker', *args], input=input, text=True).strip()


def healthy():
    for _ in range(60):
        if docker('inspect', '--format', '{{.State.Running}}', name) != 'true':
            # This fixture contains no credentials or customer data.
            raise AssertionError('Startup rejected existing settings: ' + docker('logs', name))
        probe = subprocess.run(['docker', 'exec', name, 'curl', '-sf',
                                'http://127.0.0.1:18080/api/status'],
                               text=True, capture_output=True)
        if probe.returncode == 0:
            status = json.loads(probe.stdout)
            assert status['whatsapp'] == 'Not linked'
            assert not status['has_qr']
            assert not status['relink_required']
            return
        time.sleep(1)
    raise AssertionError('Container did not become healthy')


try:
    docker('volume', 'create', volume)
    docker('run', '--rm', '-i', '--network', 'none', '--entrypoint', 'sh',
           '-v', volume + ':/data', image, '-c',
           'mkdir -p /data/state && cat > /data/state/config.json', input=config)
    for _ in range(2):
        docker('run', '-d', '--name', name, '--network', 'none', '--read-only',
               '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges:true',
               '-v', volume + ':/data', image)
        healthy()
        actual = docker('exec', name, 'sha256sum', '/data/state/config.json').split()[0]
        assert actual == digest, 'Startup rewrote the existing configuration'
        docker('stop', name)
        assert docker('inspect', '--format', '{{.State.ExitCode}}', name) == '0'
        docker('rm', name)
    print('Existing task/tracing settings, bounded concurrency, offline startup and recreation passed')
finally:
    subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['docker', 'volume', 'rm', volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
