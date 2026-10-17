"""Read-only acceptance check for explicitly sent Carbon test markers.

Run inside a Python environment with SQLite and a read-only Carbon data mount:
  python tests/whatsapp_live_check.py /data/state/carbon.sqlite --round 1
  python tests/whatsapp_live_check.py /data/state/carbon.sqlite --round 2 --expect-both
Prints only fixed markers, counts, timestamps and event names; no private bodies,
account identifiers, credentials or raw WhatsApp metadata.
"""
import argparse
import json
import sqlite3
from pathlib import Path

parser = argparse.ArgumentParser()
parser.add_argument('database', type=Path)
parser.add_argument('--round', type=int, default=2)
parser.add_argument('--expect-both', action='store_true')
args = parser.parse_args()
connection = sqlite3.connect(args.database.resolve().as_uri() + '?mode=ro', uri=True)
report = []
for device in ('IPHONE', 'MAC'):
    marker = f'Carbon-{device}-CHECK-{args.round}'
    rows = connection.execute(
        "SELECT i.id, i.state, i.created, o.operation_id FROM inbox i "
        "LEFT JOIN inbox_operations o ON o.inbox_id=i.id WHERE trim(i.text)=?", (marker,)
    ).fetchall()
    results = []
    for inbox, state, created, operation in rows:
        events = connection.execute(
            "SELECT event FROM activity WHERE operation_id=? ORDER BY id", (operation,)
        ).fetchall()
        # Intake diagnostics precede ingestion and therefore have no operation id.
        # Bound them to this message's canonical account and receive timestamp.
        diagnostics = connection.execute(
            "SELECT event FROM activity WHERE identity=(SELECT recipient FROM inbox WHERE id=?) "
            "AND created BETWEEN ? AND ? AND event IN ('whatsapp_linked_device','whatsapp_direct_message') "
            "ORDER BY id", (inbox, created - 2, created + 2)
        ).fetchall()
        results.append({'state': state, 'received_utc_epoch': created,
                        'events': [r[0] for r in events],
                        'nearby_ingress_diagnostics': [r[0] for r in diagnostics]})
    report.append({'marker': marker, 'count': len(rows), 'results': results})
print(json.dumps(report, indent=2))
if args.expect_both:
    assert all(r['count'] == 1 for r in report), 'Expected exactly one stored message from each device'
    assert all('message_received' in r['results'][0]['events'] for r in report), 'Missing activity event'
    assert 'whatsapp_linked_device' in report[1]['results'][0]['nearby_ingress_diagnostics'], 'Mac device normalization not observed'
