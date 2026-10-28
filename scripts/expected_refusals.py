"""Match only reviewed source/profile refusals; successful output still needs normal checks."""
import re


OPEN_CALL_ORDER_REFUSAL = "cannot preserve open call results across an operation without a tuple-order proof"


def refusal_outcome_matches(reason, outcome):
    return (type(outcome["exit_code"]) is int and outcome["exit_code"] > 0
            and outcome["stdout"] == ""
            and re.fullmatch(r"prototype [0-9]+, pc [0-9]+: " + re.escape(reason) + r"(?:\r?\n)?",
                             outcome["stderr"]) is not None)


def refusal_evidence(entry, source_sha256, profile, outcome):
    if any(type(profile.get(key)) is not int for key in ("opt", "debug", "bytecode_version")):
        return None
    refusal = entry.get("expected_refusal")
    if not refusal or entry.get("source_sha256") != source_sha256:
        return None
    if profile not in refusal["profiles"]:
        return None
    reason = refusal["reason"]
    if not refusal_outcome_matches(reason, outcome):
        return None
    return dict(reason=reason, source_sha256=source_sha256, profile=profile, **outcome)


def batch_refusals_match(outcome, expected_failures, input_files, output_path, skipped_files=()):
    """Validate the complete verbose batch log, including every failure's own diagnostic."""
    inputs = set(input_files)
    skipped = set(skipped_files)
    if (len(inputs) != len(input_files) or len(skipped) != len(skipped_files) or inputs & skipped
            or not set(expected_failures) <= inputs
            or type(outcome["exit_code"]) is not int
            or outcome["exit_code"] != int(bool(expected_failures)) or outcome["stdout"] != ""):
        return False
    log = outcome["stderr"].replace("\r\n", "\n")
    if "\r" in log or not log.endswith("\n"):
        return False
    lines = log[:-1].split("\n")
    count, failed = len(inputs) + len(skipped), len(expected_failures)
    if (len(lines) < 4 or lines[-4] != "----------------------------------------"
            or lines[-3] != f"Done: {len(inputs) - failed} decompiled, {len(skipped)} skipped (no bytecode), {failed} failed."
            or lines[-2] != f"Output: {output_path}"
            or not re.fullmatch(r"Time: [0-9]+\.[0-9]{2}s  \(" + str(count)
                                + r" files, [0-9]+\.[0-9] ms/file, [0-9]+ files/s, [1-9][0-9]* threads\)", lines[-1])):
        return False
    seen, index = set(), 0
    while index < len(lines) - 4:
        line = lines[index]
        if line.startswith("ok "):
            name = line[3:]
            if name not in inputs or name in expected_failures or name in seen:
                return False
        elif line.startswith("FAIL "):
            name = line[5:]
            diagnostic = expected_failures.get(name)
            if (name in seen or diagnostic is None or index + 1 >= len(lines) - 4
                    or not re.fullmatch(r"prototype [0-9]+, pc [0-9]+: [^\r\n]+(?:\r?\n)?", diagnostic)
                    or lines[index + 1] != "      " + diagnostic.removesuffix("\n").removesuffix("\r")):
                return False
            index += 1
        else:
            return False
        seen.add(name)
        index += 1
    return seen == inputs
