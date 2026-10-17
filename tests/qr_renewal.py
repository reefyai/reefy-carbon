"""Opt-in live transport check: an unlinked disposable container renews QR batches.

No phone is linked and no messages are sent. Requires Docker and WhatsApp network access.
Run: python tests/qr_renewal.py IMAGE
"""
import base64
import json
import subprocess
import sys
import time
import urllib.request
import uuid

name = 'carbon-qr-test-' + uuid.uuid4().hex[:12]
volume = name + '-data'
def docker(*args):
    return subprocess.check_output(['docker', *args], text=True).strip()
try:
    docker('volume', 'create', volume)
    docker('run', '-d', '--name', name, '--read-only', '--cap-drop', 'ALL',
           '-p', '127.0.0.1::18080', '-v', volume + ':/data', sys.argv[1])
    url = 'http://127.0.0.1:' + docker('port', name, '18080/tcp').split(':')[-1]
    for attempt in range(60):
        try:
            secret = docker('exec', name, 'cat', '/data/state/secrets/admin-password')
            break
        except subprocess.CalledProcessError:
            time.sleep(1)
    auth = 'Basic ' + base64.b64encode(('admin:' + secret).encode()).decode()
    viewer = str(uuid.uuid4())
    def view(opened):
        req = urllib.request.Request(url + '/api/whatsapp/pairing-view',
            data=json.dumps({'viewer':viewer,'open':opened}).encode(),
            headers={'Authorization':auth,'Content-Type':'application/json','X-Carbon-Request':'1'})
        with urllib.request.urlopen(req, timeout=5) as response:
            assert response.status == 204
    time.sleep(2)
    with urllib.request.urlopen(urllib.request.Request(url + '/api/status', headers={'Authorization':auth}), timeout=5) as response:
        assert not json.load(response)['has_qr'], 'QR generated before panel opened'
    first = None
    versions = set()
    deadline = time.monotonic() + 260
    while time.monotonic() < deadline:
        try:
            view(True)
            req = urllib.request.Request(url + '/api/status', headers={'Authorization': auth})
            with urllib.request.urlopen(req, timeout=5) as response:
                status = json.load(response)
        except (OSError, ValueError):
            time.sleep(2)
            continue
        if status.get('has_qr'):
            now = time.monotonic()
            if first is None:
                first = now
                print('Initial QR received; waiting through batch expiry.', flush=True)
            versions.add(status['qr_version'])
            if now - first > 160 and status['qr_expires_in'] > 40:
                assert len(versions) >= 3
                view(False)
                time.sleep(3)
                with urllib.request.urlopen(urllib.request.Request(url + '/api/status', headers={'Authorization':auth}), timeout=5) as response:
                    state = json.load(response)
                    assert state['whatsapp'] == 'Not linked' and not state['has_qr']
                print('On-demand QR rotation, batch renewal and close-to-pause passed.', flush=True)
                break
        time.sleep(2)
    else:
        raise AssertionError('No fresh QR batch after expiry within test deadline')
finally:
    subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['docker', 'volume', 'rm', volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
