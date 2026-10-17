#!/usr/bin/env python3
"""Render captured model exchanges without inventing reasoning or missing timing."""
import argparse
import hashlib
import json
from pathlib import Path


def fenced(value):
    text = json.dumps(value, indent=2, ensure_ascii=False)
    fence = '`' * max(3, max((len(s) for s in text.split() if s and set(s) == {'`'}), default=0) + 1)
    return f'{fence}json\n{text}\n{fence}\n'


def render(root):
    source = root / 'model-trace.json'
    calls = json.loads(source.read_text())
    report = json.loads((root / 'report.json').read_text()) if (root / 'report.json').exists() else {}
    rounds = root / 'model-rounds'
    rounds.mkdir(exist_ok=True)
    lines = ['# Full model/tool flow', '',
             'Captured provider responses only; no hidden reasoning is reconstructed. '
             'Each round links to its complete request and response, including prompts and tool schemas. '
             'Tool results below are the actual messages supplied back to the model. '
             'Missing historical timing/build information is left unknown.', '',
             f'- Model: `{report.get("model", "unknown")}`',
             f'- Rounds: {len(calls)}',
             f'- Acceptance: `{report.get("passed", "unknown")}`',
             '- [Participant conversation](conversation.md)',
             '- [Original raw trace](model-trace.json)',
             '- [Comparison metrics](comparison.json)', '',
             '| Round | Model-requested tools | HTTP |', '|---|---|---|']
    metrics = {'model': report.get('model'), 'scenario': report.get('scenario'),
               'passed': report.get('passed'), 'checks': report.get('checks'),
               'rounds': len(calls), 'tools': {}, 'usage': {}, 'timed_rounds': 0,
               'latency_ms': 0, 'trace_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
               'run_metadata': report.get('run_metadata'),
               'note': 'Compare identical fixture/prompt/source versions. Missing timing/metadata is unknown, not zero. Multiple runs are needed to assess variability.'}
    details = []
    for n, call in enumerate(calls, 1):
        request = call.get('request', {})
        response = call.get('response', {})
        messages = request.get('messages', [])
        choices = response.get('choices', []) if isinstance(response, dict) else []
        requested = []
        for choice in choices if isinstance(choices, list) else []:
            message = choice.get('message', {}) if isinstance(choice, dict) else {}
            tools = message.get('tool_calls') if isinstance(message, dict) else None
            if tools is not None:
                requested.extend(tools if isinstance(tools, list) else [None])
        names = [t.get('function', {}).get('name', '<malformed>') if isinstance(t, dict) and isinstance(t.get('function'), dict) else '<malformed>' for t in requested]
        for name in names:
            metrics['tools'][name] = metrics['tools'].get(name, 0) + 1
        usage = response.get('usage') if isinstance(response, dict) else None
        for key, value in usage.items() if isinstance(usage, dict) else []:
            if isinstance(value, (int, float)):
                metrics['usage'][key] = metrics['usage'].get(key, 0) + value
        if isinstance(call.get('elapsed_ms'), (int, float)):
            metrics['timed_rounds'] += 1
            metrics['latency_ms'] += call['elapsed_ms']
        label = f'{n:03d}'
        filename = f'round-{label}.md'
        rawname = f'round-{label}.json'
        (rounds / rawname).write_text(json.dumps(call, indent=2, ensure_ascii=False) + '\n')
        (rounds / filename).write_text(f'# Model round {n}\n\n## Complete request\n\n' + fenced(request) + '\n## Complete response\n\n' + fenced(response) + '\n## Transport metadata\n\n' + fenced({k: v for k, v in call.items() if k not in ('request', 'response')}))
        lines.append(f'| [{label}](model-rounds/{filename}) | {", ".join(names) or "No tool requested"} | {call.get("status", "missing")} |')
        block = [f'## Round {n}', '', f'[Full request and response](model-rounds/{filename}) · [JSON](model-rounds/{rawname})', '']
        trigger = next((m.get('content') for m in reversed(messages) if m.get('role') == 'user'), None)
        block += ['### Triggering message', '', fenced(trigger)]
        # Only trailing tool results belong to this model round; prior history remains in full request.
        trailing = []
        for message in reversed(messages):
            if message.get('role') != 'tool': break
            trailing.append(message)
        if trailing:
            block += ['### Tool results received', '', fenced(list(reversed(trailing)))]
        block += ['### Full model response', '', fenced(response)]
        details.extend(block)
    if not metrics['timed_rounds']:
        metrics['latency_ms'] = None
    runtime = root / 'runtime-traces'
    if runtime.exists():
        events = [(p, json.loads(p.read_text())) for p in runtime.glob('*.json')]
        events.sort(key=lambda item: (item[1].get('created', 0), item[0].name[37:]))
        runtime_lines = ['# Runtime trace events', '', 'Complete captured runtime events, including tool outcomes even when no subsequent model round occurred.', '']
        for path, event in events:
            runtime_lines += [f'## {event.get("kind")} · {event.get("operation_id")}', '', f'[Original JSON](runtime-traces/{path.name})', '', fenced(event)]
        (root / 'runtime-flow.md').write_text('\n'.join(runtime_lines))
        lines += ['', '[Complete runtime/tool events](runtime-flow.md)', '']
    (root / 'llm-flow.md').write_text('\n'.join(lines + [''] + details) + '\n')
    (root / 'comparison.json').write_text(json.dumps(metrics, indent=2, ensure_ascii=False) + '\n')
    return metrics


if __name__ == '__main__':
    parser = argparse.ArgumentParser()
    parser.add_argument('artifact_directory', type=Path)
    args = parser.parse_args()
    result = render(args.artifact_directory)
    print(f'Rendered {result["rounds"]} rounds into {args.artifact_directory / "llm-flow.md"}')
