#!/usr/bin/env python3
"""
spell_server.py — TCP сервер для исправления опечаток и раскладки.
Этап 1: проверка раскладки (лат->кир или кир->лат), если нужно
Этап 2: исправление орфографии через PySpellChecker
"""
import socket
import sys
import traceback
from spellchecker import SpellChecker

# Таблицы конвертации раскладки
ENG = "qwertyuiop[]asdfghjkl;'zxcvbnm,."
RUS = "йцукенгшщзхъфывапролджэячсмитьбю"

ENG_TO_RUS = {}
RUS_TO_ENG = {}

for eng_char, rus_char in zip(ENG, RUS):
    ENG_TO_RUS[eng_char] = rus_char
    ENG_TO_RUS[eng_char.upper()] = rus_char.upper()
    RUS_TO_ENG[rus_char] = eng_char
    RUS_TO_ENG[rus_char.upper()] = eng_char.upper()

print("Loading PySpellChecker...")
sys.stdout.flush()

try:
    spell = SpellChecker(language='ru')
    print("PySpellChecker loaded successfully")
except Exception as e:
    print(f"Error loading spell checker: {e}")
    traceback.print_exc()
    sys.exit(1)

def is_wrong_layout(word: str) -> bool:
    if not word:
        return False
    has_eng = False
    has_rus = False
    for c in word:
        if c in ENG or c.lower() in ENG:
            has_eng = True
        if c in RUS or c.lower() in RUS:
            has_rus = True
    return has_eng and not has_rus

def is_russian_text(text: str) -> bool:
    return any(c in RUS or c in RUS.upper() for c in text)

def convert_layout_to_ru(word: str) -> str:
    result = []
    for c in word:
        if c in ENG_TO_RUS:
            result.append(ENG_TO_RUS[c])
        else:
            result.append(c)
    return ''.join(result)

def fix_spelling(word: str) -> str:
    """Исправляет орфографические ошибки через PySpellChecker."""
    try:
        # Если слово уже правильное - не трогаем
        if word.lower() in spell:
            return word
        
        # Получаем кандидаты
        candidates = spell.candidates(word.lower())
        if candidates:
            # Берем первый кандидат (наиболее вероятный)
            corrected = list(candidates)[0]
            # Сохраняем регистр
            if word[0].isupper():
                corrected = corrected.capitalize()
            print(f"Spelling fixed: '{word}' -> '{corrected}'")
            sys.stdout.flush()
            return corrected
        return word
    except Exception as e:
        print(f"Error in spelling correction: {e}")
        return word

def process_word(word: str) -> str:
    """
    Двухэтапная обработка слова:
    1. Если слово набрано в неправильной раскладке — конвертируем
    2. Исправляем орфографию
    """
    if not word or len(word) < 2:
        return word
    
    original_word = word
    
    # Этап 1: проверка раскладки
    if is_wrong_layout(word):
        word = convert_layout_to_ru(word)
        print(f"Layout converted: '{original_word}' -> '{word}'")
        sys.stdout.flush()
    
    # Этап 2: исправление орфографии (только для русских слов)
    if is_russian_text(word):
        word = fix_spelling(word)
    
    return word

print("Ready on :9876")
sys.stdout.flush()

server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)

max_attempts = 5
for attempt in range(max_attempts):
    try:
        server.bind(("127.0.0.1", 9876))
        break
    except OSError as e:
        if attempt < max_attempts - 1:
            print(f"Port 9876 is busy, waiting... (attempt {attempt + 1}/{max_attempts})")
            sys.stdout.flush()
            import time
            time.sleep(2)
        else:
            raise e

server.listen(1)
print("Server listening on :9876")
sys.stdout.flush()

try:
    while True:
        try:
            conn, addr = server.accept()
            
            try:
                word = conn.recv(1024).decode().strip()
                if not word:
                    conn.send(b"")
                else:
                    answer = process_word(word)
                    conn.send(answer.encode())
            except Exception as e:
                print(f"Error processing request: {e}")
                try:
                    conn.send(b"")
                except:
                    pass
            finally:
                conn.close()
                
        except KeyboardInterrupt:
            print("\nShutting down...")
            break
        except Exception as e:
            print(f"Error accepting connection: {e}")
            continue
            
except Exception as e:
    print(f"Fatal server error: {e}")
finally:
    server.close()
    print("Server stopped")