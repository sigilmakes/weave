"""Session history for the kernel: the live view plus model-authored edits.

``history.index``/``path``/``get`` read the ordered view the host builds from
the session tree; ``edit`` writes one ``context_edit`` row and returns one line.

Wire contract (over the existing ``rlm.host_request`` transport):
* ``history.index`` receives the index options and returns the totals, the count
  of edits that no longer applied, and the rows.
* ``history.path`` receives ``limit`` and returns previewed entries, the total
  the view holds, and how many were shown.
* ``history.get`` receives one encoded selector plus ``limit`` and returns the
  entries, the total the selector names, and how many were shown.
* ``history.edit`` receives the changes and returns the one-line status.

Every method is a host round trip, so every method is a coroutine.
"""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import Any, Literal, NamedTuple, TypedDict, Union

__all__ = [
    "Change",
    "Entry",
    "HistoryEntries",
    "HistoryIndex",
    "HistoryNamespace",
    "IndexRow",
    "Role",
    "Selector",
]

Role = Literal["user", "assistant", "tool", "notes"]
Selector = Union[int, str, slice]

Transport = Callable[[str, dict[str, Any]], Awaitable[Any]]


class Change(TypedDict, total=False):
    op: Literal["drop", "replace", "insert"]
    at: Selector
    text: str
    role: Role
    after: Union[int, str]


class Entry(NamedTuple):
    id: str | None
    turn: int
    role: Role
    tokens: int
    text: str | None
    truncated: bool = False


class IndexRow(NamedTuple):
    id: str | None
    turn: int
    role: Role
    tokens: int
    share: float
    preview: str


class HistoryIndex(NamedTuple):
    entries: int
    live_tokens: int
    window: int
    over_by: int
    hidden: int
    hidden_tokens: int
    skipped_edits: int
    shown: int
    rows: list[IndexRow]


class HistoryEntries(list):
    """A list of entries that also reports what a bounded region left out."""

    def __init__(
        self,
        entries: list[Entry],
        total: int | None = None,
        shown: int | None = None,
        skipped_edits: int = 0,
    ) -> None:
        super().__init__(entries)
        self.total = len(self) if total is None else total
        self.shown = len(self) if shown is None else shown
        self.skipped_edits = skipped_edits

    @property
    def remainder(self) -> int:
        return max(0, self.total - self.shown)

    @property
    def note(self) -> str | None:
        if self.remainder == 0:
            return None
        return f"showing {self.shown} of {self.total} entries ({self.remainder} more)"


async def _native_request(kind: str, payload: dict[str, Any]) -> Any:
    from . import host_request

    return await host_request(kind, payload)


def _selector_payload(selector: Any, what: str = "selector") -> dict[str, Any]:
    if isinstance(selector, bool):
        raise TypeError(f"history {what} must be an int, an entry id, or a slice, not bool")
    if isinstance(selector, int):
        return {"kind": "position", "value": selector}
    if isinstance(selector, str):
        return {"kind": "id", "value": selector}
    if isinstance(selector, slice):
        if selector.step is not None:
            raise TypeError(f"history {what} does not take a step")
        return {"kind": "range", "start": selector.start, "end": selector.stop}
    raise TypeError(
        f"history {what} must be an int, an entry id, or a slice, got {type(selector).__name__}"
    )


def _target_payload(target: Any) -> Any:
    if isinstance(target, slice):
        if target.step is not None:
            raise TypeError("history edit ranges do not take a step")
        return [target.start, target.stop]
    if isinstance(target, (int, str)) and not isinstance(target, bool):
        return target
    raise TypeError(
        f"history edit 'at' must be an int, an entry id, or a slice, got {type(target).__name__}"
    )


def _point_payload(target: Any, op: str) -> Any:
    if isinstance(target, slice):
        raise TypeError(f"history edit '{op}' takes one entry, not a region")
    if isinstance(target, (int, str)) and not isinstance(target, bool):
        return target
    raise TypeError(
        f"history edit '{op}' 'at' must be an int or an entry id, got {type(target).__name__}"
    )


def _anchor_payload(after: Any) -> Any:
    if isinstance(after, (int, str)) and not isinstance(after, bool):
        return after
    raise TypeError(
        f"history edit 'after' must be an int or an entry id, got {type(after).__name__}"
    )


def _change_payload(change: Any) -> dict[str, Any]:
    if not isinstance(change, dict):
        raise TypeError(f"history edit change must be a dict, got {type(change).__name__}")
    payload = dict(change)
    op = payload.get("op")
    if op == "drop":
        payload["at"] = _target_payload(payload.get("at"))
    elif op == "replace":
        payload["at"] = _point_payload(payload.get("at"), "replace")
    elif op == "insert":
        payload["after"] = _anchor_payload(payload.get("after", -1))
    return payload


def _entry_from_payload(payload: Any) -> Entry:
    if not isinstance(payload, dict):
        raise RuntimeError("history returned an invalid entry")
    entry_id = payload.get("id")
    turn = payload.get("turn")
    role = payload.get("role")
    tokens = payload.get("tokens")
    text = payload.get("text")
    if entry_id is not None and not isinstance(entry_id, str):
        raise RuntimeError("history returned an invalid entry identity")
    if not isinstance(turn, int) or isinstance(turn, bool):
        raise RuntimeError("history returned an invalid entry turn")
    if role not in ("user", "assistant", "tool", "notes"):
        raise RuntimeError("history returned an invalid entry role")
    if not isinstance(tokens, int) or isinstance(tokens, bool):
        raise RuntimeError("history returned an invalid entry token count")
    if text is not None and not isinstance(text, str):
        raise RuntimeError("history returned invalid entry text")
    truncated = payload.get("truncated", False)
    if not isinstance(truncated, bool):
        raise RuntimeError("history returned an invalid entry cut flag")
    return Entry(
        id=entry_id, turn=turn, role=role, tokens=tokens, text=text, truncated=truncated
    )


def _row_from_payload(payload: Any) -> IndexRow:
    if not isinstance(payload, dict):
        raise RuntimeError("history.index returned an invalid row")
    entry_id = payload.get("id")
    turn = payload.get("turn")
    role = payload.get("role")
    tokens = payload.get("tokens")
    share = payload.get("share")
    preview = payload.get("preview")
    if entry_id is not None and not isinstance(entry_id, str):
        raise RuntimeError("history.index returned an invalid row identity")
    if not isinstance(turn, int) or isinstance(turn, bool):
        raise RuntimeError("history.index returned an invalid row turn")
    if role not in ("user", "assistant", "tool", "notes"):
        raise RuntimeError("history.index returned an invalid row role")
    if not isinstance(tokens, int) or isinstance(tokens, bool):
        raise RuntimeError("history.index returned an invalid row token count")
    if not isinstance(share, (int, float)) or isinstance(share, bool):
        raise RuntimeError("history.index returned an invalid row share")
    if not isinstance(preview, str):
        raise RuntimeError("history.index returned an invalid row preview")
    return IndexRow(
        id=entry_id,
        turn=turn,
        role=role,
        tokens=tokens,
        share=float(share),
        preview=preview,
    )


def _int_field(payload: dict[str, Any], name: str) -> int:
    value = payload.get(name)
    if not isinstance(value, int) or isinstance(value, bool):
        raise RuntimeError(f"history.index returned an invalid {name}")
    return value


class HistoryNamespace:
    """The session's history, bound to one session identity.

    Positions address the live view, which is what ``path`` returns and what a
    request is built from. Ids address the record: an entry an edit dropped is
    still reachable by id. An edit's selectors all resolve against the view as
    it stood before the call, so one change cannot invalidate another.

    ``notes`` entries are visible to ``path``/``get``/``index`` but never sent
    to the provider.
    """

    def __init__(self, session: str, *, transport: Transport = _native_request) -> None:
        if not isinstance(session, str) or not session:
            raise ValueError("history namespace requires a non-empty session identity")
        self._session = session
        self._transport = transport

    @property
    def session(self) -> str:
        return self._session

    async def index(
        self,
        order: Literal["tokens", "recent", "oldest", "turn"] = "tokens",
        *,
        limit: int = 20,
        role: Role | None = None,
        query: str | None = None,
        include_hidden: bool = False,
    ) -> HistoryIndex:
        if order not in ("tokens", "recent", "oldest", "turn"):
            raise ValueError(f"history.index order must be tokens, recent, oldest, or turn, got {order!r}")
        if not isinstance(limit, int) or isinstance(limit, bool) or limit < 1:
            raise ValueError("history.index limit must be a positive int")
        if role is not None and role not in ("user", "assistant", "tool", "notes"):
            raise ValueError(f"history.index role must be user, assistant, tool, or notes, got {role!r}")
        if query is not None and not isinstance(query, str):
            raise TypeError("history.index query must be a str")
        payload = await self._transport(
            "history.index",
            {
                "session": self._session,
                "order": order,
                "limit": limit,
                "role": role,
                "query": query,
                "include_hidden": include_hidden,
            },
        )
        rows = payload.get("rows")
        if not isinstance(rows, list):
            raise RuntimeError("history.index returned an invalid rows list")
        return HistoryIndex(
            entries=_int_field(payload, "entries"),
            live_tokens=_int_field(payload, "live_tokens"),
            window=_int_field(payload, "window"),
            over_by=_int_field(payload, "over_by"),
            hidden=_int_field(payload, "hidden"),
            hidden_tokens=_int_field(payload, "hidden_tokens"),
            skipped_edits=_int_field(payload, "skipped_edits"),
            shown=_int_field(payload, "shown"),
            rows=[_row_from_payload(row) for row in rows],
        )

    async def path(self, *, limit: int = 500) -> HistoryEntries:
        if not isinstance(limit, int) or isinstance(limit, bool) or limit < 1:
            raise ValueError("history.path limit must be a positive int")
        payload = await self._transport(
            "history.path", {"session": self._session, "limit": limit}
        )
        entries = payload.get("entries")
        if not isinstance(entries, list):
            raise RuntimeError("history.path returned an invalid entries list")
        return HistoryEntries(
            [_entry_from_payload(entry) for entry in entries],
            total=_int_field(payload, "total"),
            shown=_int_field(payload, "shown"),
            skipped_edits=_int_field(payload, "skipped_edits"),
        )

    async def get(self, selector: Selector, *, limit: int = 200) -> HistoryEntries:
        if not isinstance(limit, int) or isinstance(limit, bool) or limit < 1:
            raise ValueError("history.get limit must be a positive int")
        payload = await self._transport(
            "history.get",
            {
                "session": self._session,
                "selector": _selector_payload(selector),
                "limit": limit,
            },
        )
        entries = payload.get("entries")
        if not isinstance(entries, list):
            raise RuntimeError("history.get returned an invalid entries list")
        return HistoryEntries(
            [_entry_from_payload(entry) for entry in entries],
            total=_int_field(payload, "total"),
            shown=_int_field(payload, "shown"),
        )

    async def edit(self, changes: list[Change]) -> str:
        if not isinstance(changes, (list, tuple)) or not changes:
            raise TypeError("history.edit changes must be a non-empty list")
        payload = await self._transport(
            "history.edit",
            {"session": self._session, "changes": [_change_payload(change) for change in changes]},
        )
        status = payload.get("status")
        if not isinstance(status, str):
            raise RuntimeError("history.edit returned an invalid status line")
        return status
