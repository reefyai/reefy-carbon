We’re building Carbon, an AI agent built for multiple people and the private knowledge - from community conversations to company information. Try it at the Miami AI Hackathon: send a WhatsApp message to +1 (305) 525-8062 and ask about teams, notable projects, photos and more - knowledge you won’t find on the public web.

## What we built at the hackathon

At the [Miami AI Hackathon](https://luma.com/aihackathon26) on October 17, 2026, we ran Carbon on a Reefy mini-PC as a WhatsApp guide to the event and added:

- Grok sign-in. Attach your xAI Grok subscription on the Language model page and Carbon calls Grok directly, without the Reefy LLM proxy. The token stays on the device and refreshes itself.
- Admin changes from the web page. Adding or removing an admin applies at once, and the last admin can't be removed.
- Larger requests. Carbon can read several files for one answer instead of failing.
- A daily limit message. Someone who hits the limit is told once when replies resume.

## How the event bot works

An admin sends the event files to Carbon on WhatsApp (schedule, FAQ, team profiles, venue photos) and shares each one with everyone. Anyone can then ask about teams, projects or the venue, and Carbon answers from those files or sends the photos.

Next we want OpenAI Codex sign-in and a way to publish a whole set of event files in one message.

## Run it

```sh
docker build -t reefy-carbon:local .
CARBON_IMAGE=reefy-carbon:local ./start-carbon.sh
```

Open http://127.0.0.1:18082, link WhatsApp under Communication Channels and attach Grok under Language model. Run the tests with `cargo test --locked`.

More: [technical guide](docs/technical-guide.md), [deployment](docs/deployment.md), [implementation status](docs/implementation-status.md), [testing](docs/testing.md).
