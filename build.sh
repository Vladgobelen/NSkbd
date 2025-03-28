#!/bin/bash

set -e  # Прерывать выполнение при ошибках

# 1. Клонирование и сборка Rust-проекта
echo "Клонирование репозитория NSkbd..."
if [ ! -d "NSkbd" ]; then
    git clone https://github.com/Vladgobelen/NSkbd.git
fi

echo "Сборка Rust-проекта..."
cd NSkbd || exit 1
cargo build --release
cd ..

# 4. Копирование файлов в текущую директорию
echo "Копирование исполняемых файлов..."
cp NSkbd/target/release/NSKeyboardLayoutSwitcher ./

# 5. Очистка временных файлов
echo "Очистка временных файлов..."
rm -rf NSkbd

echo "=============================================="
echo "Сборка завершена успешно!"
echo "Исполняемые файлы находятся в текущей директории:"
echo "  NSKeyboardLayoutSwitcher - основной бинарник"
echo "=============================================="
