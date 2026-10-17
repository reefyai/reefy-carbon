"""Exercise real container startup, authentication and persistent credentials.

Run: python3 tests/container_smoke.py reefy-carbon:local
Creates and removes only uniquely named test containers/volumes.
"""
import base64
import json
import subprocess
import sys
import time
import uuid
import urllib.error
import urllib.request

image = sys.argv[1]
name = 'carbon-test-' + uuid.uuid4().hex[:12]
volume = name + '-data'


def docker(*args):
    return subprocess.check_output(['docker', *args], text=True).strip()


def start():
    docker('run', '-d', '--name', name, '--read-only', '--cap-drop', 'ALL',
           '--security-opt', 'no-new-privileges:true', '-p', '127.0.0.1::18080',
           '-v', volume + ':/data', image)
    port = docker('port', name, '18080/tcp').split(':')[-1]
    url = 'http://127.0.0.1:' + port
    for _ in range(60):
        try:
            with urllib.request.urlopen(url + '/healthz', timeout=2) as response:
                assert response.read() == b'ok'
            return url
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            time.sleep(1)
    raise AssertionError('Container never became healthy')


def password():
    return docker('exec', name, 'cat', '/data/state/secrets/admin-password')


try:
    docker('volume', 'create', volume)
    url = start()
    assert docker('exec', name, 'id', '-u') == '10001'
    docker('exec', name, 'test', '!', '-e', '/usr/local/bin/carbon-pdf')
    secret = password()
    assert len(secret) == 64
    for path in ('/', '/api/status', '/api/activity', '/api/approval-requests'):
        try:
            urllib.request.urlopen(url + path, timeout=2)
            raise AssertionError('Unauthenticated management request succeeded')
        except urllib.error.HTTPError as error:
            assert error.code == 401
    request = urllib.request.Request(url + '/api/status', headers={
        'Authorization': 'Basic ' + base64.b64encode(('admin:' + secret).encode()).decode()
    })
    with urllib.request.urlopen(request, timeout=2) as response:
        data = json.load(response)
        assert 'private_text_notes' in data['capabilities']
        assert 'attachment_storage' in data['capabilities']
        assert 'image_analysis' in data['capabilities']
        assert 'file_replies' in data['capabilities']
        assert 'document_analysis' in data['capabilities']
        assert 'resource_search' in data['capabilities']
        assert 'pending_reviews' in data['capabilities']
        assert 'folder_read_inheritance' in data['capabilities']
    docker('stop', name)
    assert docker('inspect', '--format', '{{.State.ExitCode}}', name) == '0'
    docker('rm', name)
    start()
    assert password() == secret, 'Recreation rotated the existing password'
    docker('stop', name)
    docker('rm', name)
    # Switch the existing installation using only its persistent config.
    docker('run', '--rm', '--entrypoint', 'sh', '-v', volume + ':/data', image,
           '-c', 'sed -i \'s/"basic"/"optional_basic"/\' /data/state/config.json && '
                 'mv /data/state/secrets/admin-password /data/state/secrets/admin-password.disabled')
    url = start()
    with urllib.request.urlopen(url + '/api/status', timeout=2) as response:
        assert response.status == 200
        state = json.load(response)
        assert 'private_text_notes' in state['capabilities']
        assert not state['has_qr']
        assert state['whatsapp'] == 'Not linked'
    docker('exec', name, 'test', '!', '-e', '/data/state/secrets/admin-password')
    assert docker('exec', name, 'cat', '/data/state/secrets/admin-password.disabled') == secret
    mutation = urllib.request.Request(url + '/api/admins/bootstrap', data=b'[]',
                                     headers={'Content-Type': 'application/json'})
    try:
        urllib.request.urlopen(mutation, timeout=2)
        raise AssertionError('Mutation without CSRF header succeeded')
    except urllib.error.HTTPError as error:
        assert error.code == 403
    for marked, expected in ((False, 403), (True, 400)):
        headers = {'Content-Type': 'application/json'}
        if marked:
            headers['X-Carbon-Request'] = '1'
        req = urllib.request.Request(url + '/api/whatsapp/pair-code',
                                     data=b'{"phone_number":"invalid"}', headers=headers)
        try:
            urllib.request.urlopen(req, timeout=2)
            raise AssertionError('Invalid pairing request succeeded')
        except urllib.error.HTTPError as error:
            assert error.code == expected
    for marked, expected in ((False, 403), (True, 409)):
        req = urllib.request.Request(url + '/api/whatsapp/unlink', data=b'{}',
                    headers={'X-Carbon-Request': '1'} if marked else {})
        try:
            urllib.request.urlopen(req, timeout=2)
            raise AssertionError('Unlinked session accepted unlink')
        except urllib.error.HTTPError as error:
            assert error.code == expected
    with urllib.request.urlopen(url, timeout=2) as response:
        page=response.read()
        assert b'Get linking code' in page
    with urllib.request.urlopen(url + '/api/activity', timeout=2) as response:
        assert response.headers['Cache-Control'] == 'no-store'
        events = json.load(response)['events']
        assert len([e for e in events if e['event'] == 'app_started']) >= 3
        assert all('message' in e and 'level' in e for e in events)
        assert secret not in json.dumps(events)
    with urllib.request.urlopen(url + '/api/approval-requests', timeout=2) as response:
        assert response.headers['Cache-Control'] == 'no-store'
        assert json.load(response) == {'requests': []}
    print('Required/optional authentication, CSRF, non-root startup, SIGTERM and persistence passed')
finally:
    subprocess.run(['docker', 'rm', '-f', name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    subprocess.run(['docker', 'volume', 'rm', volume], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
