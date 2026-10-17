# Carbon — Multiplayer AI

<img src="docs/images/carbon-logo.png" alt="Carbon logo" width="80">

One AI agent. Multiple people. Shared context.

<img src="docs/images/carbon-network.jpg" alt="Multiple people connected to the same Carbon AI agent" width="600">

Carbon is an LLM-powered agent that works with multiple people’s data and messages. Think OpenClaw or Meta Muse, with multiple people able to talk to the same agent.

At the [Miami AI Hackathon](https://luma.com/aihackathon26) on **October 17, 2026**, we’re building Carbon and exploring what multiplayer AI can do for communities, families and businesses.

## Run it on a local mini-PC

<img src="docs/images/reefy-mini-pc-v2.webp" alt="Mini-PC with Reefy branding resting on a hand" width="480">

Carbon runs on a local mini-PC powered by [Reefy OS](https://reefy.ai), with SSH, Docker, Tailscale and LLMs ready for hacking.

A mini-PC will be provided at the hackathon—there’s no need to buy one. The event page also links to a [mini-PC listed at $229](https://a.co/d/08CMx102).

## What can I do at the hackathon?

Pick a stream that interests you. No prior skills are needed—we’ll learn together.

### Core features

- **Telegram:** add another messaging channel alongside WhatsApp.
- **Email transport:** let people send requests and receive replies by email.
- **LLM subscriptions:** add web sign-in for model subscriptions alongside API access. ✅ Done for xAI Grok, see [Built at the hackathon](#built-at-the-hackathon-october-17-2026).
- **Mixture of Models:** explore how combining different models could lower running costs.
- **Device benchmarks:** measure memory, CPU and disk use to help Carbon run on smaller devices.

### Marketing

- **Launch story:** make a launch video and prepare Hacker News, Reddit and Product Hunt posts.
- **Social outreach:** write X posts and explore influencer outreach.
- **Investor outreach:** introduce Carbon’s vision and explain what we’re building.

### Use cases

- **Everyday journeys:** build and demonstrate ways Carbon could help communities, families and businesses.
- **Broken AC demo:** find local AC professionals, compare quotes and arrange an appointment authorized by the resident.

## Built at the hackathon (October 17, 2026)

This work came from the **LLM subscriptions** stream. We used it to turn Carbon into an event assistant for the hackathon itself, running on a Reefy mini-PC.

**At a glance:**

- **Grok sign-in:** attach an xAI Grok subscription from the web page, with no proxy needed.
- **Web admin console:** add or remove admins with one click.
- **Bigger requests:** reading many files at once no longer fails.
- **Daily limit notice:** people are told when they hit the limit instead of meeting silence.
- **Event assistant demo:** answers questions about the hackathon and sends venue photos.

### Sign in with your Grok subscription

Carbon no longer needs the Reefy LLM proxy to talk to a model.

1. Open the web page, go to **Language model** and find **LLM Providers**.
2. Click **Attach** next to **xAI Grok**, open the link, sign in and confirm the code.
3. Pick a model such as `xai/grok-4.7` and click **Save model**.

The sign-in is saved privately on the device in `/data/state/secrets/llm-providers.json`, and it refreshes on its own. The web page never shows the token. Grok models (`xai/…`) are called directly. Everything else still goes through the Reefy LLM proxy, and if nothing is attached Carbon works exactly as before. **OpenAI Codex** is listed as coming soon.

### Manage admins from the web page

On the **Admin** page, **Add admin** and **Remove admin** take effect immediately: the web page is the operator console. The last admin can't be removed. Other permission requests are still approved in WhatsApp.

### Bigger questions no longer fail

Asking Carbon to read several files at once used to end with "I couldn't complete this request". Carbon now allows:

- up to 12 tool calls per step, with extra calls skipped instead of failing the request;
- 8 steps per request, where the last step always gives an answer;
- requests up to 256 KB.

### A clear message at the daily limit

When someone reaches the daily message limit, Carbon replies once to say so and that replies resume after midnight UTC, then stays quiet. Before, it just stopped answering. You set the limits in `/data/state/config.json` with `daily_user_limit` and `daily_global_limit`. Our demo device uses 500 per person and 5,000 in total.

### Demo: a bot that knows the hackathon

1. An admin sends the event information to Carbon on WhatsApp as Markdown files: schedule, challenges, FAQ, team profiles and the teams index.
2. The admin asks Carbon to share each file with everyone, then replies **approve** to each request. Share one file at a time, because approving one request cancels the others that are still waiting.
3. Venue photos go in as WhatsApp **documents** with clear names such as `hackathon-inside-1.jpg`, so Carbon can find them by name, and are shared the same way.
4. Anyone can then message the bot: "Which teams are competing?" or "How does the hackathon look inside?" Carbon answers from the shared files and sends the photos.

### Try it

```sh
docker build -t reefy-carbon:local .
CARBON_IMAGE=reefy-carbon:local ./start-carbon.sh
```

Open http://127.0.0.1:18082, link WhatsApp under **Communication Channels**, then attach Grok under **Language model**. We also run this build on our Reefy mini-PC.

### More detail

- [Admin requests from the web UI](docs/admin-web-requests.md) and [managing approval admins](docs/admin-management.md)
- [First release limits and behavior](docs/first-release.md)
- Code: `src/providers.rs` (Grok sign-in and routing), `src/llm.rs` (model limits), `src/store.rs` (daily limit notice), `src/permissions.rs` (web admin changes)
- Tests: `cargo test --locked` (214 pass) and `node tests/ui_state.cjs`

## What else could Carbon do?

### Communities & cities

<img src="docs/images/communities.webp" alt="Homeowner and AC technician discussing an outdoor air-conditioning unit" width="640">

- **“My AC is broken.”** Find trusted local professionals using profiles and resident feedback, negotiate quotes and book a visit.
- **Community knowledge:** answer parking, insurance and permit questions from HOA documents, and schedule visits within community rules.

### Families

<img src="docs/images/families.webp" alt="Parents and children planning together around a kitchen table" width="640">

- **Shared family context:** coordinate children’s schedules and family goals, track learning and explore colleges around each child’s interests.
- **Less household admin:** organize medical appointments, insurance and bills while keeping sensitive information private.

### Businesses

<img src="docs/images/businesses.webp" alt="Colleagues coordinating a customer request around a shared laptop" width="640">

- **Service without lost context:** carry customer requests across specialists while preserving the history and knowledge needed at each handoff.
- **Knowledge at the right level:** keep leadership information confidential while giving interns useful, approved runbooks.
- **Customer agents are coming:** help businesses handle requests from agents such as Muse.

## Technical documentation

For architecture, diagrams, permissions, storage and implementation details, see the [technical guide](docs/technical-guide.md).

- [Deployment and configuration](docs/deployment.md)
- [Current implementation status](docs/implementation-status.md)
- [Testing and E2E coverage](docs/testing.md)
