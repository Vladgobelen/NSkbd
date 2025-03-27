#!/bin/bash

# Переходим в директорию, где находится скрипт
cd "$(dirname "$0")" || {
    echo "Ошибка перехода в директорию скрипта" >&2
    exit 1
}

# Проверяем существование файла
if [ ! -x "./NSKeyboardLayoutSwitcher" ]; then
    echo "Ошибка: файл NSKeyboardLayoutSwitcher не найден или нет прав на выполнение" >&2
    echo "Текущая директория: $(pwd)" >&2
    exit 1
fi

# Запускаем
exec ./NSKeyboardLayoutSwitcher --add