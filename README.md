# NSKeyboardLayoutSwitcher

Автоматический переключатель раскладки клавиатуры для Linux с запоминанием раскладки для каждого окна.

## Установка

1. Установите зависимости:
```bash
sudo apt install xdotool x11-utils xkblayout-state  # deb
sudo emerge -av x11-misc/xdotool x11-misc/xkblayout-state x11-apps/xprop  # gentoo
```

2. Соберите и установите:
```bash
git clone https://github.com/Vladgobelen/NSkbd
cd NSkbd
cargo build --release
cd target/release/ 
chmod +x /usr/local/bin/NSKeyboardLayoutSwitcher
```

## Использование

### Основные команды
```bash
# Запуск сервиса
./NSKeyboardLayoutSwitcher

# Добавить окно в конфиг
./NSKeyboardLayoutSwitcher --add
# или
sh NSkbd.sh
```

### Настройка горячих клавиш
Добавьте в ~/.config/sxhkd/sxhkdrc:
```bash 
ctrl + shift  {q}
    sh /путь/к/NSkbd.sh
```

## Конфигурация
- Настройки: `~/.config/NSKeyboardLayout/config.json`
- Логи: `~/.config/NSKeyboardLayout/kbd.log`

## Лицензия
MIT

## Использование
Берем в фокус окно, включаем нужную раскладку, жмем настроенный хоткей - окно сохранится
