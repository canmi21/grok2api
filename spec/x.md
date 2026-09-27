# X, through the agent

grok2api will serve X data -- posts, users, searches -- read by the agent with the tools xAI runs
for it. Not an X API client: the agent reads X, and grok2api tells it what shape to answer in. The
point is the division of labor: however X's own interfaces or xAI's tools change, grok2api asks for
the same shape and the model maps what it sees onto it. The endpoints are not designed yet; this
file holds what is established so far.

Measured against grok 1.0.41, on 2026-09-27.

## The tools are xAI's, run on its side

The CLI's default agent sends the model two server-side tools beside its own functions,
`{"type": "web_search"}` and `{"type": "x_search"}`. `x_search` gives the model four tools:

- `x_thread_fetch(post_id)` -- a post with its context: author, timestamp, engagement, media URLs
  on `pbs.twimg.com`, a quoted post nested in full, the conversation id.
- `x_keyword_search(query, limit <= 10, mode Top|Latest)` -- X's advanced search, operators and all:
  `from:`, `since:`/`until:`, `conversation_id:`, `quoted_tweet_id:`, `filter:images`,
  `min_faves:` and the rest.
- `x_semantic_search(query, limit <= 10, from_date, to_date, usernames, exclude_usernames,
min_score_threshold)`.
- `x_user_search(query, count)` -- users: id, name, handle, avatar, bio, followers, verification.

The model was asked for their definitions verbatim and for one raw result from each; the answers
are the source of the list above. A post fetched this way matched the post on X exactly, down to
its two image URLs and its quoted post.

**The web is not the way in.** Before `x_search` was found, the model reached X through
`web_search` and `web_fetch`: a search index days behind, fetches of x.com that a third-party
reader had already been blocked from, and one question that took 28 searches and 608k input
tokens. `x_search` answered four questions in one turn for 7k.

## Turning it on is a denylist

An agent profile's `tools` allowlist cannot name `x_search`: the name is not recognized, and an
unrecognized name makes the CLI keep its full tool set (bridge.md). Naming `web_search` alone
drops `x_search` with everything else. What works is the other direction: the full set, less
everything but `x_search`, with the profile's `disallowedTools` -- camel case; `disallowed_tools`
is silently ignored. The shell tool has to be named as `run_terminal_cmd` for the denial to hold.
What reaches the model is then `search_tool` and `x_search`, about 4.9k tokens of context.

`toolOverrides` in `session/new`'s `_meta`, which `initialize` advertises, did not turn it on.

## The results reach the model and nothing else

`x_search` runs on xAI's side. Its results go into the model's context and appear in no stream:
the agent reports the call's arguments and never its output, and the upstream Responses stream
carries none of it either. So the data leaves only as the model's own answer, and every field
grok2api returns is one the model wrote down.

That settles the format: the answer is constrained with an output schema (api.md, "Structured
output"), which the CLI enforces. JSON is the right shape because it is the one the constraint
holds the model to, not because the tool speaks it -- it does not; its raw output is a fixed text
layout.

## Freshness is Cache-Control, and the cache is the CDN's

grok2api keeps no cache of X data. Every request asks the agent again, and the response says how
long it may be kept, so a CDN in front does the caching:

- **A post over an hour old is `immutable`.** An X post can be edited only in the hour after it is
  posted, so past that its content does not change.
- **Anything younger, or with nothing to date it by, is kept 15 minutes.**

Engagement counts keep moving after the hour; a response marked immutable carries them as they
were when it was fetched, which is the price of the CDN being allowed to keep it.
