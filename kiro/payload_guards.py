# -*- coding: utf-8 -*-
"""
Payload size guard for Kiro API requests.

The Kiro API rejects oversized payloads with 400
"Input content length exceeds threshold." (reason:
CONTENT_LENGTH_EXCEEDS_THRESHOLD). That name is not a wire-byte count: on
runtime.us-east-1.kiro.dev / generateAssistantResponse the reject boundary
tracks cl100k tokens of the compact JSON. claude-opus-5: 800_000 Hangul
pass, 1_000_000 fail. This module provides:
- Pre-flight token (and legacy byte) checking
- Auto-trimming of oldest history entries to fit under the limit
"""

import base64
import binascii
import io
import json
from dataclasses import dataclass
from typing import Any, Dict, Optional


@dataclass
class PayloadTrimStats:
    """Statistics from a payload trim operation."""

    original_bytes: int
    final_bytes: int
    original_entries: int
    final_entries: int
    trimmed: bool
    original_tokens: int = 0
    final_tokens: int = 0
    images_stripped: int = 0


class PayloadTooLargeError(Exception):
    """Raised when a payload exceeds the limit and auto-trimming is disabled.

    Kiro answers an oversized payload with CONTENT_LENGTH_EXCEEDS_THRESHOLD,
    which names neither the size nor the limit. Failing here instead keeps the
    actual numbers in the message so the caller can act on them.
    """

    payload_bytes: int
    limit_bytes: int
    payload_tokens: int
    limit_tokens: int
    unit: str

    def __init__(
        self,
        payload_size: int,
        limit: int,
        *,
        unit: str = "bytes",
        payload_bytes: Optional[int] = None,
        payload_tokens: Optional[int] = None,
    ) -> None:
        self.unit = unit
        if unit == "tokens":
            self.payload_tokens = payload_size
            self.limit_tokens = limit
            self.payload_bytes = payload_bytes or 0
            self.limit_bytes = 0
            quantity = "tokens"
            unit_word = "token"
        else:
            self.payload_bytes = payload_size
            self.limit_bytes = limit
            self.payload_tokens = payload_tokens or 0
            self.limit_tokens = 0
            quantity = "bytes"
            unit_word = "byte"
        super().__init__(
            f"Request payload is {payload_size} {quantity}, over the {limit} {unit_word} limit Kiro accepts. "
            f"Shorten the conversation or send fewer tools. Set AUTO_TRIM_PAYLOAD=true to drop the "
            f"oldest history instead (this silently loses earlier context)."
        )


def _payload_json(payload: Dict[str, Any]) -> str:
    return json.dumps(payload, ensure_ascii=False, separators=(",", ":"))


def measure_payload(payload: Dict[str, Any]) -> tuple[int, int]:
    """Return (tokens, bytes) from a single serialization of the payload.

    check_payload_tokens() and check_payload_size() each serialized the payload
    independently, so the pre-flight guard paid the dump twice per request.
    """
    serialized = _payload_json(payload)
    from kiro.tokenizer import count_tokens

    tokens = count_tokens(serialized, apply_claude_correction=False, model="claude-haiku-4.5")
    return tokens, len(serialized.encode("utf-8"))


def check_payload_size(payload: Dict[str, Any]) -> int:
    """Return the serialized UTF-8 byte size of the compact JSON payload.

    ensure_ascii=False matches the decoded Unicode the upstream tokenizer sees
    after JSON parse. The default True would count a Hangul syllable as the 6
    bytes of a \\uXXXX escape instead of one cl100k token.
    """
    return len(_payload_json(payload).encode("utf-8"))


def payload_token_limit_for_model(model_id: str) -> int:
    """Return the pre-flight token cap.

    Single default: 800_000, the largest claude-opus-5 Hangul JSON measured to
    pass (1_000_000 returned CONTENT_LENGTH_EXCEEDS_THRESHOLD). Override with
    KIRO_MAX_PAYLOAD_TOKENS.
    """
    from kiro.config import KIRO_MAX_PAYLOAD_TOKENS

    return KIRO_MAX_PAYLOAD_TOKENS


def check_payload_tokens(payload: Dict[str, Any]) -> int:
    """Return cl100k tokens of the compact JSON, without the CJK slope correction.

    Measured 2026-08-23 against runtime.us-east-1.kiro.dev generateAssistantResponse
    (claude-haiku-4.5, no tools): a Hangul JSON of 195_000 chars returned 200, and
    200_000 chars returned 400 CONTENT_LENGTH_EXCEEDS_THRESHOLD. Repeated ASCII
    ``x`` passed at 1_550_000 chars (~193_750 cl100k tokens) and failed at
    1_575_000 (~196_875). Cycling ``abcdefghijklmnopqrstuvwxyz`` of 1_550_000
    chars failed, so the limit is tokenizer units, not wire bytes or Unicode
    scalars. The Claude CJK slope (1.15) is a local estimator for usage display
    and must not be applied here: it would reject the Hangul payload that passed.
    """
    from kiro.tokenizer import count_tokens

    return count_tokens(_payload_json(payload), apply_claude_correction=False, model="claude-haiku-4.5")


def _strip_empty_tool_uses(history: list) -> None:
    """Remove empty toolUses arrays in-place (Kiro quirk)."""
    for entry in history:
        assistant = entry.get("assistantResponseMessage")
        if assistant and "toolUses" in assistant and assistant["toolUses"] == []:
            del assistant["toolUses"]


def _align_to_user_message(history: list) -> list:
    """Ensure history starts with a userInputMessage entry."""
    while history and "userInputMessage" not in history[0]:
        history.pop(0)
    return history


def _repair_orphaned_tool_results(history: list, current_message: Optional[Dict[str, Any]] = None) -> None:
    """
    Remove orphaned toolResults that reference toolUseIds not present
    in the preceding assistant message. Preserve orphaned text content
    inline with a marker.
    """
    entries = history + ([current_message] if current_message else [])
    for i, entry in enumerate(entries):
        user_msg = entry.get("userInputMessage")
        if not user_msg:
            continue

        ctx = user_msg.get("userInputMessageContext")
        if not ctx or "toolResults" not in ctx:
            continue

        # Collect toolUseIds from the preceding assistant message
        valid_ids = set()
        if i > 0:
            prev_assistant = entries[i - 1].get("assistantResponseMessage")
            if prev_assistant:
                for tu in prev_assistant.get("toolUses", []):
                    tool_use_id = tu.get("toolUseId")
                    if tool_use_id:
                        valid_ids.add(tool_use_id)

        kept = []
        orphaned_text_parts = []
        for tr in ctx["toolResults"]:
            if tr.get("toolUseId") in valid_ids:
                kept.append(tr)
            else:
                # Preserve text content from orphaned results
                content = tr.get("content")
                if isinstance(content, list):
                    for part in content:
                        if isinstance(part, dict) and part.get("text"):
                            orphaned_text_parts.append(part["text"])
                elif isinstance(content, str) and content:
                    orphaned_text_parts.append(content)

        if len(kept) != len(ctx["toolResults"]):
            if kept:
                ctx["toolResults"] = kept
            else:
                del ctx["toolResults"]
                if not ctx:
                    del user_msg["userInputMessageContext"]

            # Append orphaned text to user message content
            if orphaned_text_parts:
                marker = "\n[trimmed tool result] " + "; ".join(orphaned_text_parts)
                current_content = user_msg.get("content", "")
                user_msg["content"] = current_content + marker


def _over_limit(payload: Dict[str, Any], max_bytes: Optional[int], max_tokens: Optional[int]) -> bool:
    if max_tokens is not None and check_payload_tokens(payload) > max_tokens:
        return True
    if max_bytes is not None and check_payload_size(payload) > max_bytes:
        return True
    return False


def _drop_pairs_by_estimate(
    payload: Dict[str, Any],
    history: list,
    max_bytes: Optional[int],
    max_tokens: Optional[int],
    total_tokens: int,
    total_bytes: int,
) -> None:
    """Drop old pairs using estimated cost, without any extra full pass.

    The original loop called _over_limit() per iteration, and each call
    serialized and tokenized the whole payload: 113 iterations on a 3 MB payload
    cost ~44s before the request even left.

    Each entry's byte cost comes from its own cheap serialization, and its token
    share is prorated from the payload's real token total. Tokenizing every entry
    would be more precise but costs another full pass; the proration is close
    enough and the caller's exact check corrects the rest.

    The 3% margin keeps a proration error from leaving the payload just above the
    cap, which would force exact iterations - the expensive ones.
    """
    if not history:
        return

    entry_bytes = [len(_payload_json(entry).encode("utf-8")) + 1 for entry in history]
    history_bytes = sum(entry_bytes)
    if history_bytes <= 0:
        return

    tokens_per_byte = total_tokens / total_bytes if total_bytes else 0.0
    remaining_tokens = float(total_tokens)
    remaining_bytes = total_bytes

    token_target = max_tokens * 0.97 if max_tokens is not None else None

    index = 0
    while index < len(history):
        over_tokens = token_target is not None and remaining_tokens > token_target
        over_bytes = max_bytes is not None and remaining_bytes > max_bytes
        if not (over_tokens or over_bytes):
            break
        for _ in range(2):
            if index < len(history):
                remaining_tokens -= entry_bytes[index] * tokens_per_byte
                remaining_bytes -= entry_bytes[index]
                index += 1

    if index:
        del history[:index]


def _known_over_limit(
    tokens: int,
    nbytes: int,
    max_bytes: Optional[int],
    max_tokens: Optional[int],
) -> bool:
    if max_tokens is not None and tokens > max_tokens:
        return True
    if max_bytes is not None and nbytes > max_bytes:
        return True
    return False


def _current_user_input(payload: Dict[str, Any]) -> Optional[Dict[str, Any]]:
    conversation_state = payload.get("conversationState")
    if not isinstance(conversation_state, dict):
        return None
    current_message = conversation_state.get("currentMessage")
    if not isinstance(current_message, dict):
        return None
    user_input = current_message.get("userInputMessage")
    if not isinstance(user_input, dict):
        return None
    return user_input


def _drop_oldest_current_images_until_fit(
    payload: Dict[str, Any],
    max_bytes: Optional[int],
    max_tokens: Optional[int],
) -> None:
    """Remove oldest current-turn images until the payload fits."""
    user_input = _current_user_input(payload)
    if user_input is None:
        return
    images = user_input.get("images")
    if not isinstance(images, list) or not images:
        return
    while images and _over_limit(payload, max_bytes, max_tokens):
        images.pop(0)
    if not images:
        user_input.pop("images", None)


def _shrink_kiro_image(image: Dict[str, Any], max_edge: int = 1280, quality: int = 70) -> bool:
    """Re-encode one Kiro image as a smaller JPEG. Returns True if it shrank."""
    source = image.get("source")
    if not isinstance(source, dict):
        return False
    raw_b64 = source.get("bytes")
    if not isinstance(raw_b64, str) or not raw_b64:
        return False
    try:
        from PIL import Image
    except ImportError:
        return False
    try:
        original = base64.b64decode(raw_b64, validate=True)
        with Image.open(io.BytesIO(original)) as decoded:
            rgb = decoded.convert("RGB")
            width, height = rgb.size
            longest = max(width, height)
            if longest > max_edge:
                scale = max_edge / longest
                rgb = rgb.resize((max(1, int(width * scale)), max(1, int(height * scale))))
            buffer = io.BytesIO()
            rgb.save(buffer, format="JPEG", quality=quality, optimize=True)
    except (OSError, ValueError, binascii.Error):
        return False
    shrunk = base64.b64encode(buffer.getvalue()).decode("ascii")
    if len(shrunk) >= len(raw_b64):
        return False
    image["format"] = "jpeg"
    source["bytes"] = shrunk
    return True


def _shrink_current_images_until_fit(
    payload: Dict[str, Any],
    max_bytes: Optional[int],
    max_tokens: Optional[int],
) -> None:
    """Downscale remaining current-turn images when history trim is not enough."""
    user_input = _current_user_input(payload)
    if user_input is None:
        return
    images = user_input.get("images")
    if not isinstance(images, list) or not images:
        return
    for max_edge, quality in ((1280, 70), (720, 50)):
        for image in images:
            if not _over_limit(payload, max_bytes, max_tokens):
                return
            if isinstance(image, dict):
                _shrink_kiro_image(image, max_edge=max_edge, quality=quality)


def _fit_current_images(
    payload: Dict[str, Any],
    max_bytes: Optional[int],
    max_tokens: Optional[int],
) -> None:
    """Shrink, then drop, current-turn images. History trim cannot remove them."""
    if not _over_limit(payload, max_bytes, max_tokens):
        return
    _shrink_current_images_until_fit(payload, max_bytes, max_tokens)
    if _over_limit(payload, max_bytes, max_tokens):
        _drop_oldest_current_images_until_fit(payload, max_bytes, max_tokens)


HISTORY_IMAGE_PLACEHOLDER = "[image omitted: trimmed to fit the Kiro payload limit]"

# cl100k tokens per base64 character, set just above the ~0.71 measured on
# PNG screenshots. Overestimating what each strip frees makes the loop stop
# early and re-measure, instead of discarding recent images that could stay.
_B64_TOKENS_PER_CHAR_ESTIMATE = 0.75


def _history_image_slots(history: list) -> list[tuple[Dict[str, Any], int]]:
    """Return (userInputMessage, base64 length) for history entries with images, oldest first."""
    slots = []
    for entry in history:
        user_msg = entry.get("userInputMessage") if isinstance(entry, dict) else None
        if not isinstance(user_msg, dict):
            continue
        images = user_msg.get("images")
        if not isinstance(images, list) or not images:
            continue
        size = 0
        for image in images:
            source = image.get("source") if isinstance(image, dict) else None
            data = source.get("bytes") if isinstance(source, dict) else None
            if isinstance(data, str):
                size += len(data)
        slots.append((user_msg, size))
    return slots


def _strip_history_image(user_msg: Dict[str, Any]) -> None:
    count = len(user_msg.pop("images", None) or [])
    note = HISTORY_IMAGE_PLACEHOLDER if count == 1 else f"{HISTORY_IMAGE_PLACEHOLDER} x{count}"
    content = user_msg.get("content") or ""
    user_msg["content"] = f"{content}\n{note}" if content else note


def _strip_history_images_until_fit(
    payload: Dict[str, Any],
    history: list,
    max_bytes: Optional[int],
    max_tokens: Optional[int],
    total_tokens: int,
    total_bytes: int,
) -> tuple[int, int, int]:
    """Drop base64 images from the oldest history turns before any text is trimmed.

    Screenshots dominate agent payloads: 58 PNGs are ~18.7M base64 chars, ~13M
    cl100k tokens, against ~0.4M tokens of text. Trimming whole turns to make
    room for them discarded the entire conversation while keeping the pixels.
    Old images are replaced by a text placeholder; newest images survive longest.

    Returns (images_stripped, tokens, bytes) with the exact final measurement.
    """
    slots = _history_image_slots(history)
    stripped = 0
    tokens, nbytes = total_tokens, total_bytes
    while slots and _known_over_limit(tokens, nbytes, max_bytes, max_tokens):
        token_excess = tokens - max_tokens * 0.97 if max_tokens is not None else 0
        byte_excess = nbytes - max_bytes if max_bytes is not None else 0
        freed_tokens = 0.0
        freed_bytes = 0
        while slots and (freed_tokens < token_excess or freed_bytes < byte_excess):
            user_msg, size = slots.pop(0)
            _strip_history_image(user_msg)
            stripped += 1
            freed_tokens += size * _B64_TOKENS_PER_CHAR_ESTIMATE
            freed_bytes += size
        tokens, nbytes = measure_payload(payload)
    return stripped, tokens, nbytes


def trim_payload_to_limit(
    payload: Dict[str, Any],
    max_bytes: Optional[int] = None,
    max_tokens: Optional[int] = None,
    known_tokens: Optional[int] = None,
    known_bytes: Optional[int] = None,
) -> PayloadTrimStats:
    """
    Trim oldest history entries so the payload fits under max_tokens and/or max_bytes.

    Trims in user/assistant pairs (2 entries at a time), aligns start to
    userInputMessage, and repairs orphaned toolResults after trimming.

    ``known_tokens``/``known_bytes`` reuse the measurement the caller already did
    in the pre-flight guard. Without them, measuring again costs a full
    serialization and tokenization pass over the whole payload.
    """
    if known_tokens is not None and known_bytes is not None:
        original_tokens, original_bytes = known_tokens, known_bytes
    else:
        original_tokens, original_bytes = measure_payload(payload)
    conversation_state = payload.get("conversationState", {})
    history = conversation_state.get("history")

    if not history:
        if not _known_over_limit(original_tokens, original_bytes, max_bytes, max_tokens):
            return PayloadTrimStats(
                original_bytes=original_bytes,
                final_bytes=original_bytes,
                original_entries=0,
                final_entries=0,
                trimmed=False,
                original_tokens=original_tokens,
                final_tokens=original_tokens,
            )
        _fit_current_images(payload, max_bytes, max_tokens)
        final_tokens, final_bytes = measure_payload(payload)
        return PayloadTrimStats(
            original_bytes=original_bytes,
            final_bytes=final_bytes,
            original_entries=0,
            final_entries=0,
            trimmed=final_bytes < original_bytes or final_tokens < original_tokens,
            original_tokens=original_tokens,
            final_tokens=final_tokens,
        )

    original_entries = len(history)

    # Strip empty toolUses before measuring
    _strip_empty_tool_uses(history)

    # Old screenshots go first: they cost far more than the text around them,
    # and the assistant's reply to each one already carries what it showed.
    images_stripped = 0
    current_tokens, current_bytes = original_tokens, original_bytes
    if _known_over_limit(original_tokens, original_bytes, max_bytes, max_tokens):
        images_stripped, current_tokens, current_bytes = _strip_history_images_until_fit(
            payload, history, max_bytes, max_tokens, original_tokens, original_bytes
        )

    # Trim pairs from the beginning until under limit or no history remains.
    # The per-entry estimate handles the bulk without re-tokenizing the whole
    # payload; the exact check below covers the tokenizer's boundary difference
    # and normally needs no extra iteration.
    _drop_pairs_by_estimate(payload, history, max_bytes, max_tokens, current_tokens, current_bytes)
    while history and _over_limit(payload, max_bytes, max_tokens):
        del history[:2]

    # Align to userInputMessage boundary
    _align_to_user_message(history)

    # Repair orphaned tool results after trimming
    _repair_orphaned_tool_results(history, conversation_state.get("currentMessage"))

    if not history:
        del conversation_state["history"]
        _fit_current_images(payload, max_bytes, max_tokens)

    final_tokens, final_bytes = measure_payload(payload)
    return PayloadTrimStats(
        original_bytes=original_bytes,
        final_bytes=final_bytes,
        original_entries=original_entries,
        final_entries=len(history),
        trimmed=original_entries != len(history) or images_stripped > 0,
        original_tokens=original_tokens,
        final_tokens=final_tokens,
        images_stripped=images_stripped,
    )
