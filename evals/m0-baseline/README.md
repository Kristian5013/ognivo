# M0 baseline: OpenCode 2.2.1 на трёх локальных моделях

Отправная точка ognivo: что делает готовый агент OpenCode с gpt-oss-20b, Gemma 4 26B A4B и
Qwen3.8-27B на нашем llama-server. Итоги и наблюдения — в [docs/m0/baseline.md](../../docs/m0/baseline.md).

## Что здесь лежит и что нет

| путь | что это |
|---|---|
| `harness/` | исходники стенда: прогоны, ночи, образ, оценщик, тесты. Описание — `harness/README.md` (английский, как комментарии к коду) |
| `tasks.json` | манифест 18 задач: id, роль, тип, сложность, `task_sha256` |

Сами задачи (промпты, скрытые тесты, эталонные решения, снимки репозиториев) в git не входят.
Это решение владельца от 2026-10-30: публичный репозиторий сделал бы их обучающими данными, и
eval перестал бы что-либо мерить.

- Задачи лежат в `~/.local/share/ognivo/m0-baseline/tasks/`.
- Резервная копия: `~/.local/share/ognivo/backups/m0-baseline-tasks-2026-10-30.tar.gz`. Её sha256
  записан в `tasks.json`.
- Тот же `task_sha256` (каталог задачи плюс её `_common/<group>`) попадает в каждый
  `result.json` как `versions.task_sha256`.

## Как работать со стендом

Стенд запускается из `~/.local/share/ognivo/m0-baseline/`: рядом с харнессом там лежат кеш,
результаты и рабочие копии. Здешний `harness/` — исходник. После правки его копируют туда:

```sh
rsync -a --delete --exclude __pycache__ evals/m0-baseline/harness/ ~/.local/share/ognivo/m0-baseline/harness/
~/.local/share/ognivo/m0-baseline/harness/tests/selftest.sh          # без GPU и сети
~/.local/share/ognivo/m0-baseline/harness/tests/container-smoke.sh   # Podman, без GPU
```

Идущие кампании это не задевает: каждая работает из своей замороженной копии
`runs/<stamp>/frozen/`.

Ночь запускает владелец вечером, в tmux:

```sh
~/.local/share/ognivo/m0-baseline/harness/run-night.sh --new   # первая ночь: создаёт и замораживает кампанию
~/.local/share/ognivo/m0-baseline/harness/run-night.sh         # следующие вечера: продолжает её
~/.local/share/ognivo/m0-baseline/harness/run-night.sh --status   # утром: прогресс и таблица
```

На время кампании пакеты GPU-стека и Podman замораживаются от unattended-upgrades:
список и команды — в `harness/hold-packages.txt`.
