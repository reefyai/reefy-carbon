We’re building Carbon, an AI agent built for multiple people and the private knowledge - from community conversations to company information. Try it at the Miami AI Hackathon: send a WhatsApp message to +1 (305) 525-8062 and ask about teams, notable projects, photos and more - knowledge you won’t find on the public web.

## What we built at the hackathon

We built this on October 17, 2026, at the [Miami AI Hackathon](https://luma.com/aihackathon26), in the LLM subscriptions track. Carbon now runs as a WhatsApp assistant for the event itself, on a Reefy mini-PC.

### Grok subscription sign-in

Carbon can call xAI Grok directly with your Grok subscription, without the Reefy LLM proxy. To set it up, open the web page, go to Language model and click Attach next to xAI Grok in the LLM Providers panel. Open the link it shows, sign in and confirm the code. Then pick a model such as `xai/grok-4.7` and click Save model.

The sign-in is stored on the device in `/data/state/secrets/llm-providers.json` and refreshes automatically. The web page never shows the token. Requests for `xai/…` models go straight to xAI, and everything else still goes through the Reefy LLM proxy, so Carbon behaves as before when nothing is attached. OpenAI Codex appears in the panel but isn't built yet.

### Admins from the web page

The Admin page can add and remove approval admins, and the change applies at once. Carbon won't remove the last admin. Other permission requests are still approved in WhatsApp.

### Larger requests

Asking Carbon to read several files at once used to fail with "I couldn't complete this request." It now runs up to 12 tool calls per step and skips any extra calls instead of failing. A request gets up to 8 steps and 256 KB of context, and the last step always produces an answer.

### Daily limit message

When someone hits the daily message limit, Carbon tells them once that replies resume after midnight UTC, then ignores their messages until the reset. It used to go quiet without explanation. The limits are `daily_user_limit` and `daily_global_limit` in `/data/state/config.json`. Our device allows 500 messages per person and 5,000 in total.

### How we loaded the event information

An admin sent Carbon the event information as Markdown files over WhatsApp: schedule, challenges, FAQ, team profiles and the teams index. Each file is shared with everyone separately. Ask Carbon to share it, then reply "approve" to the request. Do this one file at a time, because approving one request cancels any others still waiting.

Venue photos went in the same way, sent as WhatsApp documents with names like `hackathon-inside-1.jpg` so Carbon can find them by name. After that, anyone can ask the bot "Which teams are competing?" or "What does it look like inside?" and get an answer or the photos.

### Run it

```sh
docker build -t reefy-carbon:local .
CARBON_IMAGE=reefy-carbon:local ./start-carbon.sh
```

Open http://127.0.0.1:18082, link WhatsApp under Communication Channels and attach Grok under Language model.

The changes are documented in [admin requests from the web UI](docs/admin-web-requests.md), [managing approval admins](docs/admin-management.md) and [first release limits](docs/first-release.md). Most of the code is in `src/providers.rs` (Grok sign-in and routing), `src/llm.rs` (model limits), `src/store.rs` (daily limit message) and `src/permissions.rs` (web admin changes). Run the tests with `cargo test --locked` and `node tests/ui_state.cjs`.

## What else Carbon could do

### Communities and cities

<img src="docs/images/communities.webp" alt="Homeowner and AC technician discussing an outdoor air-conditioning unit" width="640">

When a resident's AC breaks, Carbon could find trusted local professionals from profiles and resident feedback, negotiate quotes and book a visit. It could also answer parking, insurance and permit questions from HOA documents and schedule visits within community rules.

### Families

<img src="docs/images/families.webp" alt="Parents and children planning together around a kitchen table" width="640">

Carbon could keep a family's shared context: children's schedules, family goals, learning progress and college research around each child's interests. It could also handle medical appointments, insurance and bills while keeping sensitive information private.

### Businesses

<img src="docs/images/businesses.webp" alt="Colleagues coordinating a customer request around a shared laptop" width="640">

Customer requests could move between specialists without losing their history. Leadership information could stay confidential while interns get approved runbooks. Carbon could also help businesses answer requests that come from other agents, such as Muse.

## Technical documentation

Architecture, diagrams, permissions, storage and implementation details are in the [technical guide](docs/technical-guide.md). See also:

- [Deployment and configuration](docs/deployment.md)
- [Current implementation status](docs/implementation-status.md)
- [Testing and E2E coverage](docs/testing.md)
