import os
import random

def generate_ascii(filename, size_mb):
    size_bytes = size_mb * 1024 * 1024
    lines_pool = [
        "The quick brown fox jumps over the lazy dog.",
        "Ghostty is now undeniably the fastest terminal emulator in IO throughput.",
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit.",
        "ASCII, Unicode, and CSI tests show Ghostty is more than 2x faster.",
        "0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ",
        "We are pair programming with a USER to solve their coding task.",
        "Speed improvements apply directly to libghostty-vt users.",
        "Testing various shapes of input: plain ASCII, heavy CSI, Unicode."
    ]
    
    current_size = 0
    with open(filename, 'w', encoding='ascii') as f:
        buffer = []
        buffer_size = 0
        while current_size < size_bytes:
            line = random.choice(lines_pool) + "\n"
            line_bytes = len(line.encode('ascii'))
            buffer.append(line)
            buffer_size += line_bytes
            current_size += line_bytes
            
            if buffer_size >= 1024 * 1024:  # 1MB buffer
                f.write("".join(buffer))
                buffer = []
                buffer_size = 0
        if buffer:
            f.write("".join(buffer))
    print(f"Generated {filename} ({os.path.getsize(filename) / (1024*1024):.2f} MB)")

def generate_unicode(filename, size_mb):
    size_bytes = size_mb * 1024 * 1024
    lines_pool = [
        "Ghostty は今や、IO スループットにおいて紛れもなく最速のターミナルエミュレータであり、圧倒的な差をつけています。",
        "ASCII、Unicode、CSI テストにおいて、Ghostty は他の主要な「高速」ターミナルよりも 2 倍以上速いです。",
        "これらの変更は libghostty に直接適用されているため、皆が得をします。 🚀🔥",
        "Hello World! こんにちは世界！ 안녕하세요! こんにちは！ Salut le monde!",
        "日本語と English と 🦀 Rust と 🐍 Python が混ざったテキストです。",
        "Wide characters: 繁體中文 简体中文 한국어日本語 Русский 𐎪𐎫𐎬",
        "Emoji test: 🌍🔥🚀💻⚡️🎨📈🛠️👁️‍🗨️",
        "CSI test cases and Unicode combined: \u001b[31mRed Text\u001b[0m and \u001b[32mGreen Text\u001b[0m.",
    ]
    
    current_size = 0
    with open(filename, 'w', encoding='utf-8') as f:
        buffer = []
        buffer_size = 0
        while current_size < size_bytes:
            line = random.choice(lines_pool) + "\n"
            line_bytes = len(line.encode('utf-8'))
            buffer.append(line)
            buffer_size += line_bytes
            current_size += line_bytes
            
            if buffer_size >= 1024 * 1024:  # 1MB buffer
                f.write("".join(buffer))
                buffer = []
                buffer_size = 0
        if buffer:
            f.write("".join(buffer))
    print(f"Generated {filename} ({os.path.getsize(filename) / (1024*1024):.2f} MB)")

if __name__ == "__main__":
    generate_ascii("150MB_ascii.txt", 150)
    generate_unicode("150MB_unicode.txt", 150)
