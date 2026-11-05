# The MCP server of T2's mcp scenarios: a team's workspace (issues, weather, money, docs,
# chat, calendar, CI) as one stdio server of the initialize era, with no network and no
# state. `--tools N` gives its first N tools: the first five are what the scenarios ask
# for, the next ten are their near neighbours (list_issues beside create_issue,
# get_forecast beside get_weather, ...), and the rest are the other tools of such a
# workspace. Answers are made up and fixed, so a run can be judged by them.
#
#   python3 -I workspace.py --tools 15

import json
import sys


def s(description, **extra):
    return {"type": "string", "description": description, **extra}


def obj(properties, required):
    return {"type": "object", "properties": properties, "required": required}


REPO = s("The repository, as owner/name.")
CITY = s("The city's name, in English.")

TOOLS = [
    # what the scenarios ask for
    ("create_issue", "Opens a new issue in a repository of the tracker and gives its number.",
     obj({"repo": REPO, "title": s("The issue's title."), "body": s("What the issue is about, in Markdown."),
          "labels": {"type": "array", "items": {"type": "string"}, "description": "Labels to put on it."}},
         ["repo", "title"])),
    ("get_weather", "The weather in a city now: temperature, sky and wind.",
     obj({"city": CITY, "units": {"type": "string", "enum": ["metric", "imperial"]}}, ["city"])),
    ("convert_currency", "Converts an amount of money from one currency to another at today's rate.",
     obj({"amount": {"type": "number"}, "from": s("ISO 4217 code, such as USD."), "to": s("ISO 4217 code, such as EUR.")},
         ["amount", "from", "to"])),
    ("search_docs", "Searches the team's internal documentation (handbook, runbooks, how-tos) and gives the passages that fit best.",
     obj({"query": s("Words to look for."), "limit": {"type": "integer", "minimum": 1, "maximum": 10}}, ["query"])),
    ("send_message", "Posts a message to a chat channel of the team.",
     obj({"channel": s("The channel, such as #general."), "text": s("The message.")}, ["channel", "text"])),
    # their near neighbours
    ("list_issues", "Lists the issues of a repository, newest first.",
     obj({"repo": REPO, "state": {"type": "string", "enum": ["open", "closed", "all"]}, "label": s("Only issues with this label.")},
         ["repo"])),
    ("comment_on_issue", "Adds a comment to an issue that exists.",
     obj({"repo": REPO, "number": {"type": "integer"}, "body": s("The comment, in Markdown.")}, ["repo", "number", "body"])),
    ("close_issue", "Closes an issue, with a comment if one is given.",
     obj({"repo": REPO, "number": {"type": "integer"}, "comment": s("Why it is closed.")}, ["repo", "number"])),
    ("get_forecast", "The weather forecast for a city for the next days, day by day.",
     obj({"city": CITY, "days": {"type": "integer", "minimum": 1, "maximum": 10}}, ["city", "days"])),
    ("get_exchange_rate", "Today's exchange rate between two currencies, without converting any amount.",
     obj({"base": s("ISO 4217 code."), "quote": s("ISO 4217 code.")}, ["base", "quote"])),
    ("search_issues", "Searches the issues of every repository by the words of their titles and bodies.",
     obj({"query": s("Words to look for."), "state": {"type": "string", "enum": ["open", "closed", "all"]}}, ["query"])),
    ("send_email", "Sends an email to one or more addresses.",
     obj({"to": {"type": "array", "items": {"type": "string"}}, "subject": s("The subject line."), "body": s("The text.")},
         ["to", "subject", "body"])),
    ("list_channels", "Lists the chat channels of the team, with their topics.", obj({}, [])),
    ("get_doc_page", "Gives one page of the internal documentation in full, by its path.",
     obj({"path": s("The page's path, such as security/keys.")}, ["path"])),
    ("create_calendar_event", "Puts an event in the team's calendar.",
     obj({"title": s("What the event is."), "start": s("When it starts, as ISO 8601."), "duration_minutes": {"type": "integer"},
          "attendees": {"type": "array", "items": {"type": "string"}}}, ["title", "start"])),
    # the rest of the workspace
    ("list_pull_requests", "Lists the pull requests of a repository.",
     obj({"repo": REPO, "state": {"type": "string", "enum": ["open", "closed", "merged", "all"]}}, ["repo"])),
    ("create_pull_request", "Opens a pull request from one branch into another.",
     obj({"repo": REPO, "title": s("Its title."), "head": s("The branch with the changes."), "base": s("The branch to merge into.")},
         ["repo", "title", "head", "base"])),
    ("merge_pull_request", "Merges a pull request.",
     obj({"repo": REPO, "number": {"type": "integer"}, "method": {"type": "string", "enum": ["merge", "squash", "rebase"]}},
         ["repo", "number"])),
    ("list_repositories", "Lists the repositories of an owner.", obj({"owner": s("A user or an organisation.")}, ["owner"])),
    ("get_air_quality", "The air quality index of a city now.", obj({"city": CITY}, ["city"])),
    ("get_time_zone", "The time zone of a city and its local time now.", obj({"city": CITY}, ["city"])),
    ("list_currencies", "Lists the currencies the rates are known for.", obj({}, [])),
    ("get_stock_price", "The last price of a share on its exchange.", obj({"symbol": s("The ticker, such as ACME.")}, ["symbol"])),
    ("update_doc_page", "Replaces the text of a page of the internal documentation.",
     obj({"path": s("The page's path."), "content": s("Its new text, in Markdown.")}, ["path", "content"])),
    ("search_messages", "Searches the chat's messages by their words.",
     obj({"query": s("Words to look for."), "channel": s("Only this channel.")}, ["query"])),
    ("list_messages", "Gives the last messages of a chat channel.",
     obj({"channel": s("The channel."), "limit": {"type": "integer"}}, ["channel"])),
    ("add_reaction", "Adds an emoji reaction to a chat message.",
     obj({"channel": s("The channel."), "message_id": s("The message."), "emoji": s("Its name, such as thumbsup.")},
         ["channel", "message_id", "emoji"])),
    ("list_calendar_events", "Lists the team calendar's events between two dates.",
     obj({"from": s("ISO 8601 date."), "to": s("ISO 8601 date.")}, ["from", "to"])),
    ("delete_calendar_event", "Removes an event from the team calendar.", obj({"event_id": s("The event.")}, ["event_id"])),
    ("find_free_slot", "Finds a time when all of the people given are free.",
     obj({"attendees": {"type": "array", "items": {"type": "string"}}, "duration_minutes": {"type": "integer"},
          "within_days": {"type": "integer"}}, ["attendees", "duration_minutes"])),
    ("create_task", "Adds a task to the team's task board.",
     obj({"title": s("The task."), "assignee": s("Who does it."), "due": s("ISO 8601 date.")}, ["title"])),
    ("list_tasks", "Lists the tasks of the board.",
     obj({"assignee": s("Only theirs."), "status": {"type": "string", "enum": ["todo", "doing", "done"]}}, [])),
    ("complete_task", "Marks a task of the board as done.", obj({"task_id": s("The task.")}, ["task_id"])),
    ("lookup_user", "Finds a person of the team by name or email: their handle, role and team.",
     obj({"query": s("A name or an email address.")}, ["query"])),
    ("list_users", "Lists the people of a team.", obj({"team": s("The team's name.")}, [])),
    ("get_build_status", "The state of the last CI build of a branch.",
     obj({"repo": REPO, "branch": s("The branch.")}, ["repo", "branch"])),
    ("trigger_build", "Starts a CI build of a branch.", obj({"repo": REPO, "branch": s("The branch.")}, ["repo", "branch"])),
    ("get_deploy_log", "The last lines of a service's deployment log.",
     obj({"service": s("The service."), "lines": {"type": "integer"}}, ["service"])),
    ("create_invoice", "Makes an invoice for a customer.",
     obj({"customer": s("The customer."), "amount": {"type": "number"}, "currency": s("ISO 4217 code.")},
         ["customer", "amount", "currency"])),
    ("translate_text", "Translates a text into another language.",
     obj({"text": s("The text."), "to": s("The language, such as German.")}, ["text", "to"])),
]

# per unit of USD
RATES = {"USD": 1.0, "EUR": 0.9256, "GBP": 0.7712, "JPY": 152.31, "NOK": 10.94, "CHF": 0.8641}


def answer(name, args):
    if name == "create_issue":
        labels = args.get("labels") or []
        tail = f" (labels: {', '.join(labels)})" if labels else ""
        return f"Created issue #4127 in {args.get('repo')}: {args.get('title')}{tail}"
    if name == "get_weather":
        return f"{args.get('city')}: -3 °C, light snow, wind 4 m/s from the north."
    if name == "convert_currency":
        frm, to = str(args.get("from", "")).upper(), str(args.get("to", "")).upper()
        if frm not in RATES or to not in RATES:
            return None
        amount = float(args.get("amount", 0))
        rate = RATES[to] / RATES[frm]
        return f"{amount:g} {frm} = {amount * rate:.2f} {to} at {rate:.4f} (today's rate)"
    if name == "search_docs":
        q = str(args.get("query", "")).lower()
        if "key" in q or "rotat" in q or "sign" in q:
            return ("[security/keys] Rotating the API signing key: run `keyctl rotate --grace 24h` on the "
                    "vault host. The old key stays valid for the grace period.")
        return "[handbook/start] Welcome to the team handbook: see security/, runbooks/ and howto/."
    if name == "send_message":
        return f"Posted to {args.get('channel')} (message 1762330000.0042)."
    if name.startswith(("get_", "list_", "search_", "find_", "lookup_")):
        return f"{name}: nothing found for {json.dumps(args)}."
    return f"{name}: done."


def send(message):
    sys.stdout.write(json.dumps(message) + "\n")
    sys.stdout.flush()


n = len(TOOLS)
if "--tools" in sys.argv:
    n = int(sys.argv[sys.argv.index("--tools") + 1])
offered = {name: (description, schema) for name, description, schema in TOOLS[:n]}

for line in sys.stdin:
    m = json.loads(line)
    if "id" not in m:
        continue
    method, i, p = m.get("method"), m["id"], m.get("params") or {}
    if method == "initialize":
        result = {
            "protocolVersion": p.get("protocolVersion", "2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "workspace", "version": "1"},
        }
    elif method == "tools/list":
        result = {"tools": [{"name": k, "description": d, "inputSchema": sc} for k, (d, sc) in offered.items()]}
    elif method == "tools/call":
        name, args = p.get("name"), p.get("arguments") or {}
        text = answer(name, args) if name in offered else None
        if text is None:
            result = {"isError": True, "content": [{"type": "text", "text": f"{name}: these arguments do not work: {json.dumps(args)}"}]}
        else:
            result = {"content": [{"type": "text", "text": text}]}
    elif method == "ping":
        result = {}
    else:
        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32601, "message": "Method not found"}})
        continue
    send({"jsonrpc": "2.0", "id": i, "result": result})
