#!/bin/bash

set -e  # Прерывать выполнение при ошибках

# 2. Сборка xkblayout-state
echo "Сборка xkblayout-state..."
if [ ! -d "xkblayout-state" ]; then
    git clone https://github.com/nonpop/xkblayout-state.git
fi

cd xkblayout-state || exit 1
make clean
make
cd ..

if [ -f "xkblayout-state/xkblayout-state" ]; then
    cp -f xkblayout-state/xkblayout-state ./xkblayout-state-bin
    echo "xkblayout-state собран успешно!"
else
    echo "Ошибка сборки xkblayout-state!"
    exit 1
fi

# 3. Клонирование и сборка Rust-проекта
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
cp NSkbd/config.json ./ 2>/dev/null || echo "Конфиг config.json не найден, будет создан при первом запуске"

# 5. Очистка временных файлов
echo "Очистка временных файлов..."
rm -rf xkblayout-state
rm -rf NSkbd
mv xkblayout-state-bin xkblayout-state

echo "=============================================="
echo "Сборка завершена успешно!"
echo "Исполняемые файлы находятся в текущей директории:"
echo "  NSKeyboardLayoutSwitcher - основной бинарник"
echo "  config.json - файл конфигурации (если был)"
echo "=============================================="

# 1. Установка зависимостей
echo "Установка системных зависимостей..."
echo "Не забудьте установить: libx11-dev, xdotool, x11-utils, xprop"