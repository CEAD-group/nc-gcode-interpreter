"""Helper for writing multi-line NC programs in tests."""


def nc_lines(*lines: str) -> str:
    """Join NC blocks into a program, one block per line.

    Written as varargs rather than `"\\n".join([...])` so each block can carry
    its own trailing `#` comment explaining what it contributes to the expected
    output - which is most of why these programs are written a line at a time.
    """
    return "\n".join(lines)
