from typing import Tuple
import re


def transfer_gpio(s: str) -> Tuple[int, int] | None:
    pattern = re.compile(r'GPIO(\d+)_([A-D])(\d)')
    match = pattern.match(s.upper())
    if match:
        bank = int(match.group(1))
        group = ord(match.group(2)) - ord('A')
        bank_idx = int(match.group(3))
        chip_id = bank
        line_id = group * 8 + bank_idx
        return chip_id, line_id
    return None


if __name__ == '__main__':
    from sys import argv
    args = argv[1:]
    for arg in args:
        match = transfer_gpio(arg)
        if match:
            print(f"{arg} -> gpiochip{match[0]}   line {match[1]}")
        else:
            print(f"Invalid GPIO: {arg}")