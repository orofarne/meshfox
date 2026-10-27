<!-- meshfox:canvas -->
# Выбор базы для сервиса уведомлений

<!-- meshfox:comment -->
> This is a [meshfox](https://meshfox.orofarne.net/) document — open it
> with `meshfox view` (or `meshfox tui`) for the interactive canvas.
> This note is only visible here, in a plain Markdown viewer.
<!-- /meshfox:comment -->

Обсуждали, на чём строить хранилище очереди уведомлений. Склоняемся к PostgreSQL: нагрузка небольшая, а отдельный брокер усложнит эксплуатацию.

## Варианты
<!-- meshfox:node id="options" -->

| Вариант | Плюсы | Минусы |
|---|---|---|
| PostgreSQL (`SKIP LOCKED`) | уже есть в стеке, транзакции | потолок ~1k msg/s |
| Redis Streams | быстро | отдельная персистентность |
| Kafka | масштаб | избыточно для задачи |

## Решение: PostgreSQL
<!-- meshfox:node id="decision" color="4" -->
<!-- meshfox:edge from="options" label="выбрали" -->

Очередь в таблице `notifications`, воркеры берут строки через `FOR UPDATE SKIP LOCKED`. Вернёмся к вопросу, если нагрузка превысит 500 msg/s.

## Открытые вопросы
<!-- meshfox:node id="questions" color="3" -->

- Нужны ли гарантии порядка внутри одного пользователя?
- Сколько хранить обработанные сообщения?

## Риски
<!-- meshfox:node id="risks" color="1" -->
<!-- meshfox:edge from="decision" label="цена решения" -->

- Рост таблицы без партиционирования замедлит выборку.

## Что делать дальше
<!-- meshfox:node id="actions" color="5" -->
<!-- meshfox:edge from="questions" label="блокирует" -->

- [ ] Нагрузочный тест на 1k msg/s: Аня, до пятницы
- [ ] Набросать схему таблицы с партиционированием
- [x] Согласовать выбор с командой платформы

## Источники
<!-- meshfox:node id="sources" type="group" -->

### Документация SKIP LOCKED
<!-- meshfox:node id="doc-skip-locked" type="link" -->

[PostgreSQL: SELECT ... FOR UPDATE SKIP LOCKED](https://www.postgresql.org/docs/current/sql-select.html#SQL-FOR-UPDATE-SHARE)
