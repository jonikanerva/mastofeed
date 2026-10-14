# Product Vision

## Vision

The owner reads their Mastodon home timeline in a feed reader, together with
their other subscriptions, instead of in a Mastodon client.

## Goal

The service publishes the owner's home timeline as one JSON Feed at a stable
public URL. A feed reader that polls the URL sees new statuses within one
poll interval and sees edits of recent statuses.

## Audience and environment

- One user: the owner. The feed reader is the only regular client.
- One always-on Railway service with one PostgreSQL database.
- The service reads the timeline with the owner's Mastodon access token.
  The token gives read access to the owner's account.
- The feed URL is public by design. Anyone who knows it can read the
  timeline. The owner accepts this.

## Core Principles

- **Feed stability.** Feed readers keep their read state by item id and can
  compare content. Item ids, item links, and the item HTML must not change
  without an owner decision.
- **Low upkeep.** The service has one job and few parts. A change that adds
  a provider, a second service, or a recurring manual task needs a reason.
- **Least data.** The service stores only what the feed shows, plus the
  status object as Mastodon sent it. It sends data to nobody except the
  Mastodon instance (the timeline request) and feed readers (the feed).

## Product Shape

1. On start and on a cron schedule (UTC, every five minutes by default), the
   service requests the newest page of the home timeline and the statuses
   newer than the newest stored status.
2. A boost becomes its original status. The service stores each status once
   and rewrites it only when a rendered value changes.
3. `/feed.json` lists the newest statuses (200 by default) oldest first.
   Each item has the status HTML, the media attachments, and an author footer.
   The item link opens the status on the owner's own instance.
4. `/` is a short landing page. `/health` reports liveness and the version.

Failure behaviour:

- A failed poll changes nothing. The next scheduled poll tries again. The
  feed continues to show the stored statuses.
- If the database is unavailable, `/feed.json` answers 503 with a problem
  document. `/health` still answers.
- An invalid setting stops the service at start, before it serves traffic.

## Non-Goals & Drift Guardrails

- No accounts, no multiple users, and no feed selection by request.
- No other output formats (RSS, Atom) unless the owner asks for them.
- No write access to Mastodon: no posting, boosting, or favouriting.
- No browser application. The landing page stays static.
- Signs of drift: a second credential, a user table, a UI framework, or a
  background job that is not the timeline poll.

## Decision Filter

- Does the change keep item ids, links, and HTML stable for feed readers?
- Does it keep the service a single-purpose, single-user feed?
- Does it add a provider, a data flow, or a recurring cost? Then the owner
  decides.
- Can the owner still revert to the previous release without a data change?

## Success Definition

- The feed reader shows new home-timeline statuses within one poll interval.
- An edited status shows the edited content after the next poll.
- An unchanged feed answers `304 Not Modified` to a conditional request.
- The deployed service runs without manual work between releases.

## Persistence and Privacy Posture

| Data or capability | Purpose | Storage or recipient | Retention or expiry |
| --- | --- | --- | --- |
| Mastodon access token | Read the home timeline | Railway service variable | Until the owner rotates it |
| Home-timeline statuses, with the full status object | Render the feed; detect edits | Railway PostgreSQL, table `mastodon_statuses` | No expiry; the feed shows the newest 200 |
| Feed document | Feed readers | Public HTTP response | Not stored |

- Forbidden data flows: the token or a status to any recipient other than
  the Mastodon instance and the feed; analytics; error-reporting services.
- Telemetry and diagnostics: the service log in Railway only. It has no
  token, URL query, or status content.
- Approved deletion policies: none. Deleting statuses needs an owner decision.
