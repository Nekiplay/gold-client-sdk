# GoldSrc studio-модели: реверс движковых и клиентских функций

Документ описывает, как Gold Client (CS 1.6) грузит и рисует студийные модели (`.mdl`), какие
функции за это отвечают, их адреса и сигнатуры. Всё получено статическим анализом в Ghidra 12.2
(PE-заголовки, RVA→файловое смещение, декомпиляция всех точек входа) на бинарниках из
`C:\Games\CS 1.6 GoldClient`.

Обозначения:

- `VA` — виртуальный адрес при базе образа `0x10000000` (у обоих модулей база одинаковая).
- `+0xNNNNNN` — смещение от базы модуля (`VA - 0x10000000` = RVA).
- ✔ — подтверждено кодом (сигнатуры/смещения видны в декомпиляции и совпадают при перекрёстной проверке).
- `?` — вывод по косвенным признакам, точное имя не подтверждено.

---

## 1. Как движок получает клиентский рендерер

Цепочка целиком:

```
hw.dll  R_StudioInit/hook  @0x10035FA0
        g_pStudioInterface = &g_DefaultStudioInterface          (по умолчанию)
        if (client_loaded && pfn) pfn(1, &g_pStudioInterface, &g_EngineStudioAPI)
                                        │
                                        ▼
client.dll  HUD_GetStudioModelInterface(version, &ppinterface, pstudio)
            export RVA 0xBA0F0 → VA 0x10BA0F0, реализация @0x10060AE0
              if (version != 1) return 0
              *ppinterface = &g_StudioInterface          (0x101966B0)
              memcpy(&g_EngineStudioAPI_copy, pstudio, 0xC0)   (0x101B2548)
              Init()                                     (@0x10060AB0)
              return 1
```

### 1.1 hw.dll

| Что | Адрес | Примечание |
|---|---|---|
| Установка интерфейса студийного рендера | `0x10035FA0` ✔ | читает указатель на `HUD_GetStudioModelInterface` из `0x1043DE4C`, флаг готовности клиента — `0x1043DB9C` |
| `g_pStudioInterface` (`r_studio_interface_t *`) | `0x10271B04` | **точка подмены рендерера** |
| `g_EngineStudioAPI` (`engine_studio_api_t`, 0xC0 байт) | `0x10271B08` | статически инициализированная таблица 48 функций |
| `g_DefaultStudioInterface` | `0x10271AF0` | `{1, 0x1009C1E0, 0x1009BCD0}` — собственный рендерер движка, используется, если клиент не подсунул свой |
| Указатель на клиентский экспорт | `0x1043DE4C` | заполняется при загрузке `client.dll` |

`g_DefaultStudioInterface` — удобный **референс**: `0x1009C1E0` (DrawModel) и `0x1009BCD0`
(DrawPlayer) это полные движковые реализации, независимые от CS-кода.

### 1.2 client.dll

| Что | Адрес | Примечание |
|---|---|---|
| `HUD_GetStudioModelInterface` (экспорт) | `0x10BA0F0` | тонкая обёртка/форвард, тело ниже |
| `HUD_GetStudioModelInterface` (код) | `0x10060AE0` | 55 байт, см. псевдокод выше |
| `Init()` | `0x10060AB0` | инициализирует рендерер и грузит 10 моделей игроков |
| `g_StudioInterface` | `0x101966B0` | `{int version=1, fn DrawModel, fn DrawPlayer}` |
| копия `engine_studio_api_t` | `0x101B2548` | 0xC0 байт, 1:1 копия `hw.dll+0x271B08` |
| объект `CStudioModelRenderer` | `0x101F51C8` | статический, в BSS (в файле нули) |
| таблица моделей игроков | `0x101966C0`, шаг 8, 10 записей | `{model_t* model; const char* name;}` |

Модели (порядок как в `Init`): `models/player.mdl`, `leet`, `gign`, `vip`, `gsg9`, `guerilla`,
`arctic`, `sas`, `terror`, `urban`. Загружаются через `engine_studio_api_t+0xBC`
(`Mod_ForName(1, name)`).

---

## 2. `r_studio_interface_t`

```c
typedef struct r_studio_interface_s {
    int  version;                                   // 0x00, движок принимает только 1
    int (*StudioDrawModel)( int flags );            // 0x04
    int (*StudioDrawPlayer)( int flags, struct player_info_s *pplayer ); // 0x08
} r_studio_interface_t;                             // sizeof = 0x0C
```

Клиентская реализация:

| Слот | Адрес | Что делает |
|---|---|---|
| `+0x04` DrawModel | `0x10060A90` (18 байт) | просто вызывает `0x100AEF60` |
| `+0x08` DrawPlayer | `0x10060A30` (87 байт) | обёртка: если сущность == «view entity», включает/выключает эффекты (`0x1005ED10`, `0x1005F1E0`, `0x1005F4A0`), затем вызывает `*(void**)(obj+0x6C)` — там лежит `0x100AFE60` |

То есть реальные обработчики — `0x100AEF60` (`CStudioModelRenderer::StudioDrawModel`) и
`0x100AFE60` (`StudioDrawPlayer`).

---

## 3. `engine_studio_api_t` — 48 слотов

Таблица в `hw.dll` по `+0x271B08`, клиент копирует её себе в `+0x1B2548`. Порядок и сигнатуры
восстановлены по декомпиляции каждого слота и по вызовам из клиента (`client.dll+0x100AEF60` и др.).

| off | hw.dll | размер | сигнатура / смысл |
|---|---|---|---|
| `+0x00` | `0x100C7D40` | 9 | ✔ `void *Mem_Calloc(int count, int size)` → `__calloc_base` |
| `+0x04` | `0x1011E240` | 139 | ✔ `void Mem_Free(void *p)` → `Cache_Free`/`Cache_UnlinkLRU` |
| `+0x08` | `0x100B8CE0` | 415 | ✔ `void *COM_LoadFile(const char *path, int usehunk)` |
| `+0x0C` | `0x1009D830` | 61 | `void Mod_SetFlags(void)` — ставит текущей модели флаги `|2`, `|4` |
| `+0x10` | `0x10069D40` | 56 | ✔ `studiohdr_t *Mod_Extradata(model_t *m)` → `*(m+0x184)` |
| `+0x14` | `0x10033350` | 12 | ✔ `model_t *Mod_ForIndex(int index)` → `0x100485B0` (массив `0x12F895C8[512]`, грузит по требованию) |
| `+0x18` | `0x1009CF00` | 6 | ✔ `void *GetCurrentEntity(void)` → `*(0x1078E470)`, «текущая сущность рендера» |
| `+0x1C` | `0x1009CF10` | 35 | ✔ `entity_state_t *GetEntityState(int i)` — `&0x12F90A18 + i*0xA0`, границы `[0, 0x12F8FE14)` |
| `+0x20` | `0x1009CF40` | 48 | ✔ `cl_entity_t *GetEntityByIndex(int i)` — `0x12E15ED8 + [0x12E15A4C]*0x4320 + i*0x154` |
| `+0x24` | `0x1009CF70` | 6 | `int *GetParsecountPtr(void)` → константа `0x12F8FE30` |
| `+0x28` | `0x1009CF80` | 45 | ✔ `void GetTimes(int *viewent, float *cl_time, float *cl_oldtime)` — читает `0x1078E474` (int), `0x12E15EB0`, `0x12E15EB8` (float) |
| `+0x2C` | `0x1009D340` | 90 | `void *Mod_UnloadModel(void *)` — через vtable `+0x54`, освобождает |
| `+0x30` | `0x1009D290` | 169 | ✔ `void GetViewInfo(vec3 origin, vec3 up, vec3 right, vec3 forward)` (`0x1078E9BC`, `0x1078E44C`, `0x1078E440`, `0x1078E464`) |
| `+0x34` | `0x1009D3A0` | 6 | `int GetRenderFlagsPtr(void)` → `*(0x1074BD08)` |
| `+0x38` | `0x1009D3B0` | 23 | `void GetLightPointers(void **a, void **b)` → `0x1288C5B4`, `0x12890738` |
| `+0x3C` | `0x1009D3D0` | 23 | ✔ `void GetPlayerColors(float *top, float *bottom)` → `1.0, 1.0` |
| `+0x40` | `0x1009D260` | 6 | `void *GetPtr_129F2A58(void)` |
| `+0x44` | `0x1009D270` | 6 | `void *GetPtr_129F1258(void)` |
| `+0x48` | `0x10019A40` | 3 | ✔ `int ReturnZero(void)` → `0` |
| `+0x4C` | `0x1009D280` | 6 | `void *GetPtr_129F4258(void)` |
| `+0x50` | `0x1009CFB0` | 31 | ✔ `void SetupModel(int modelindex, void **ppsubmodel, void **ppmodel)` → `R_StudioSetupModel` (`0x10098E10`), результат `0x129829C0` / `0x129829BC` |
| `+0x54` | `0x100943C0` | 294 | ✔ `int StudioCheckBBox(void)` — считает bbox и `R_CullBox` (`0x100C67B0`) |
| `+0x58` | `0x10099560` | 3191 | ✔ `void StudioSetupLighting(entity, alight_t *out)` — dlighы, `entity+0xB48` origin |
| `+0x5C` | `0x1009A1E0` | 514 | `void StudioSetupDlights(void)` — обход списка динамических источников `0x1044328C..0x1044378C`, шаг `0x28` |
| `+0x60` | `0x10098CD0` | 315 | ✔ `void StudioSetupLightTransform(lightparams_t *p)` — умножает матрицы костей (`0x129F1278`, шаг `0xC`) |
| `+0x64` | `0x1009B4C0` | 584 | `void StudioDrawGlowShell(void)` — мигание/шелл (`entity+0xBC0 & 1`) |
| `+0x68` | `0x10096BE0` | 921 | `void StudioDrawModelInternal(void)` — начало отрисовки меша |
| `+0x6C` | `0x10096F80` | 702 | `void StudioDrawMeshes(void)` |
| `+0x70` | `0x10097240` | 2313 | `void StudioDrawMesh(void)` — основной триангулятор/`glDraw` |
| `+0x74` | `0x10098A10` | 698 | `void StudioDrawTextures(int a, int b)` |
| `+0x78` | `0x1009D3F0` | 21 | ✔ `void SetPlayerModelColors(int top, int bottom)` → `0x1288C5B0`, `0x128597AC` |
| `+0x7C` | `0x1009D140` | 287 | `int StudioGetPlayerModel(void)` — выбор модели игрока по байту |
| `+0x80` | `0x1009A3F0` | 527 | `void StudioRenderFinal(void)` — финальные GL-состояния |
| `+0x84` | `0x1009D450` | 6 | `int Get_129829B0(void)` |
| `+0x88` | `0x1009D460` | 13 | ✔ `void Set_129829B0(int)` — указатель на текущий `studiohdr` |
| `+0x8C` | `0x1009D470` | 13 | ✔ `void Set_129F4888(int)` — указатель на `studiohdr` для рендера |
| `+0x90` | `0x1009D480` | 13 | ✔ `void Set_129829C8(int)` — указатель на `submodel`/меш |
| `+0x94` | `0x1009CFD0` | 229 | `void StudioInitLight(void)` |
| `+0x98` | `0x1009D0C0` | 25 | `void StudioSetTextureState(void)` — GL `0xBE2` |
| `+0x9C` | `0x1009D120` | 30 | `void StudioCopyViewOrigin(void)` — копирует origin вида в `0x129F1258` |
| `+0xA0` | `0x1009D490` | 13 | `int StudioGetRenderMode(void)` → 1 или 2 |
| `+0xA4` | `0x1009D5B0` | 295 | `void StudioBindTexture(int)` — `glBindTexture` + combine |
| `+0xA8` | `0x1009D4A0` | 262 | `void StudioSetTextureMode(int mode)` |
| `+0xAC` | `0x1009D410` | 53 | ✔ `void StudioSetRenderAmt(int amt)` — `entity+0x2FC`, `0x129F488C` |
| `+0xB0` | `0x100192C0` | 3 | ✔ `int ReturnZero2(void)` → `0` |
| `+0xB4` | `0x1009D6E0` | 275 | `void StudioDrawModelByIndex(int index, int, int, int, int)` |
| `+0xB8` | `0x1009D800` | 38 | ✔ `void SetRenderTarget(int p)` → `0x129829A8`, `glCullFace` |
| `+0xBC` | `0x1009D870` | 37 | ✔ `model_t *Mod_ForName(int load, const char *name)` — **порядок аргументов обратный HLSDK** |

Замечание по `+0xBC`: в HLSDK заголовок объявляет `Mod_ForName(name, load)`, а здесь реализация
читает `arg0` как `load` (`mov cl,[ebp+8]`) и `arg1` как `name` (`mov edx,[ebp+0xC]`), и клиент
вызывает именно `f(1, name)`. Для хука это важно — не перепутать.

### 3.1 Как клиент дёргает api в `StudioDrawModel`

`client.dll+0x100AEF60` (`flags` в `arg1`):

```
m_pCurrentEntity = api->GetCurrentEntity()                     // +0x18
m_bIsPlayer      = (m_pCurrentEntity == GetViewEntity())        // client 0x101B0290
api->GetTimes(&m_clTimeZone, &m_clTime, &m_clOldTime)          // +0x28
api->GetViewInfo(m_vUp/Right/..., 4 вектора)                   // +0x30
api->GetPlayerColors(&m_fTopColor, &m_fBottomColor)            // +0x3C
m_pRenderModel  = m_bIsPlayer ? MapPlayerModel(entity->model) : entity->model   // entity+0xB94
m_pStudioHeader = api->Mod_Extradata(m_pRenderModel)           // +0x10
api->Set_129F4888(m_pStudioHeader)                             // +0x8C
api->Set_129829C8(m_pCurrentEntity->submodel?)                 // +0x90
vtable[6](0)                                                   // 0x100ABD10
if (flags & 1) { api->StudioCheckBBox() ... ++m_pModelsDrawn } // +0x54
vtable[7](-1)      // 0x100ACB20 StudioSetupBones
vtable[9]()        // 0x100AE130
vtable[8]()        // 0x100B02E0
api->SetRenderTarget(...)                                      // +0xB8
...
api->StudioSetupLighting(m_pCurrentEntity, &lighting)          // +0x58
api->StudioSetupDlights()                                      // +0x5C
api->StudioSetupLightTransform(&lighting)                      // +0x60
vtable[10](m_pRenderModel)  // SetPlayerModel                  // 0x100AE1E0
vtable[19]()                // 0x100B0410
```

---

## 4. `CStudioModelRenderer` (client.dll) — объект, поля, vtable

Объект: `0x101F51C8`. В конструкторе (`0x100AA7F0`) в `[obj+0]` пишется vtable `0x10168F94`
(26 слотов).

### 4.1 Подтверждённые смещения полей (thiscall, `this` = объект)

| смещение | тип | смысл |
|---|---|---|
| `+0x00` | `void**` | vtable (`0x10168F94`) |
| `+0x08` | `float` | `m_clTime` (заполняется `api+0x28`) |
| `+0x10` | `float` | `m_clOldTime` |
| `+0x18` | `void*` | сохранённый указатель счётчика (на время DrawPlayer) |
| `+0x20` | `int` | `m_nFrameCount` |
| `+0x30` | `entity*` | `m_pCurrentEntity` (из `api+0x18`) |
| `+0x34` | `bool` | `m_bIsPlayer` (сущность == view entity) |
| `+0x38` | `model_t*` | `m_pRenderModel` |
| `+0x40` | `int` | `m_nRenderAmt`, уходит в `api+0xB8` |
| `+0x4C` | `studiohdr_t*` | `m_pStudioHeader` |
| `+0x58` / `+0x5C` | `int` | `m_nTopColor` / `m_nBottomColor` (из `entity+0x2E4`) |
| `+0x6C` | `fn ptr` | вызов `StudioDrawPlayer`, ставится при инициализации (`0x100AFE60`) |
| `+0x406C` / `+0x4070` | `float` | два цвета игрока (`api+0x3C`) |
| `+0x4074` | `vec3` | `m_vUp` |
| `+0x4080` | `vec3` | `m_vRight` |
| `+0x408C` | `vec3` | `m_vNormal` |
| `+0x4098` | `vec3` | `m_vRenderOrigin` (4 вектора пишет `api+0x30`) |
| `+0x40A4` | `int*` | `m_pStudioModelCount` |
| `+0x40A8` | `int*` | `m_pModelsDrawn` |
| `+0x40AC` | `float(*)[3][4]` | `m_protationmatrix` |
| `+0x40B0` | `float(*)[4][4]` | `m_paliastransform` |
| `+0x40B4` | `float(*)[MAXSTUDIOBONES][3][4]` | **`m_pbonetransform`** — матрицы костей, шаг `0x30` на кость |
| `+0x40B8` | `float(*)[MAXSTUDIOBONES][3][4]` | `m_plighttransform` |

`MAXSTUDIOBONES = 128`, кость = 12 float (`0x30`).

### 4.2 Vtable `0x10168F94` (26 слотов, thiscall)

| # | off | адрес | смысл |
|---|---|---|---|
| 0 | `+0x00` | `0x100AA9A0` | ? (тонкий метод, вызывается из `+0x10`-входа) |
| 1 | `+0x04` | `0x100AA6D0` | ✔ `Init()` |
| 2 | `+0x08` | `0x100AA7F0` | ✔ конструктор/`Term()` (пишет vtable) |
| 3 | `+0x0C` | `0x100AEF60` | ✔ `StudioDrawModel(int flags)` |
| 4 | `+0x10` | `0x100AFE60` | ✔ `StudioDrawPlayer(int flags, player_info_t*)` |
| 5 | `+0x14` | `0x100ABB90` | ? |
| 6 | `+0x18` | `0x100ABD10` | ? (`(0)` перед рендером) |
| 7 | `+0x1C` | `0x100ACB20` | ✔ `StudioSetupBones()` (вызов с `-1`) |
| 8 | `+0x20` | `0x100B02E0` | ✔ `StudioRenderModel()` |
| 9 | `+0x24` | `0x100AE130` | ? |
| 10 | `+0x28` | `0x100AE1E0` | `SetPlayerModel(model_t*)` |
| 11 | `+0x2C` | `0x100AC360` | ? |
| 12 | `+0x30` | `0x100AC9F0` | ? |
| 13 | `+0x34` | `0x100AC880` | ? |
| 14 | `+0x38` | `0x100AB720` | ? |
| 15 | `+0x3C` | `0x100AA9D0` | ? |
| 16 | `+0x40` | `0x100AAC80` | ? |
| 17 | `+0x44` | `0x100AB5C0` | ? |
| 18 | `+0x48` | `0x100AC3D0` | ? |
| 19 | `+0x4C` | `0x100B0410` | ? (в конце DrawModel) |
| 20 | `+0x50` | `0x100B0AC0` | ? |
| 21 | `+0x54` | `0x100B07B0` | ? |
| 22 | `+0x58` | `0x100B08E0` | ? |
| 23 | `+0x5C` | `0x100ABC30` | ? |
| 24 | `+0x60` | `0x100AF500` | ? |
| 25 | `+0x64` | `0x100AF7E0` | ? |

Неназванные слоты — это подсобные `Studio*`-методы (установка оружия/attachment, работа с
`mstudioanimseq`-группами и т.п.); адреса стабильны, имена восстановим по мере надобности.

---

## 5. Движковая часть (hw.dll): от отрисовки к геометрии

| Функция | Адрес | Что делает |
|---|---|---|
| `R_StudioSetupModel` | `0x10098E10` (125 байт) | ✔ выбирает `submodel` по `bodypart`/`sequence`: `hdr->bodypartindex + bodypart*0x4C + submodel*4` → `basesubmodel`, пишет `psubmodel`/`pmodel` |
| `R_StudioSetupSkin`? | `0x10098E90` (683) | скин/`skinref`-таблица |
| `R_StudioRenderModel`? | `0x10099140` (903) | обход `nummodels` и вызов мешей |
| `R_StudioDrawMesh`? | `0x100994D0` (134) | тонкая обёртка над `+0x68..+0x74` |
| `R_StudioSetupBones`-хелперы | `0x100984E0`, `0x10098710`, `0x10098820` | скелет: `bone*0x70`, `anim`-значения, `bonecontroller` |
| `SV_StudioSetupBones` (серверная копия) | `0x10092C70` | та же математика костей — удобный эталон для сверки |

Смещения в `studiohdr_t`, подтверждённые этими функциями: `+0x8C` — `numbones`, `+0xA8` —
`seqindex` (шаг `0xB0`), `bone+0x20` — `parent`, кость — `0x70` байт.

---

## 6. Загрузчик моделей (`Mod_*`)

| Функция | Адрес | Сигнатура |
|---|---|---|
| `Mod_FindName` | `0x10069FF0` (495) | `model_t *Mod_FindName(const char *name)` — хэш-поиск, `"%s: NULL name"` |
| `Mod_Load` | `0x1006A2F0` (899) | `model_t *Mod_LoadModel(model_t *m, int load...)` — читает файл, `LoadVertexes/LoadBones/...` |
| `Mod_ForName` | `0x1009D870` (37) | ✔ `model_t *Mod_ForName(int load, const char *name)` = `FindName` + при `load` `Mod_Load` + флаги `|2`,`|4` |
| `Mod_Extradata` | `0x10069D40` (56) | ✔ `studiohdr_t *Mod_Extradata(model_t *m)` → `m+0x184`, `"Mod_Extradata: caching failed for %s"` |
| `Mod_ForIndex` | `0x100485B0` (145) | ✔ `model_t *Mod_ForIndex(int i)` — `0x12F895C8[i]`, `i < 0x200`, грузит по требованию, `fs_precache_timings` |
| `Mod_LoadStudioModel`-кластер | `0x10092C10` / `0x10092C70` | инициализация studio-модели, `SV_StudioSetupBones` |
| загрузчик `T.mdl` | `0x100486BE` | `mod_studio` — регистрация/`Mod_LoadStudioModel`, обработка `"T.mdl"` |
| массив моделей | `0x12F895C8` | 512 (`0x200`) указателей |

Поля `model_t` (нужные для студии): `+0x40` флаги (`1`=загружена, `2`/`4`=студийные),
`+0x44` тип (`1`=studio, `3`=?), `+0x184` — `studiohdr_t*` (extradata).

---

## 7. Формат `.mdl` (GoldSrc, версия 10) — проверено по файлам и коду

Раздел переписан: то, что здесь, сверено **побайтово с моделями из установки**
(`cstrike/models/player/gign/gign.mdl`, `bag.mdl`, `chick.mdl`) и с кодом загрузчика.
Про HLSDK-заголовки: они для этого билда не подходят (см. список отличий ниже).

`studiohdr_t`, размер ровно `0xF4`:

| off | поле | | off | поле |
|---|---|---|---|---|
| `0x00` | `id` = `"IDST"` | | `0x8C` | ✔ `numbones` |
| `0x04` | `version` = `10` (0x0A) | | `0x90` | ✔ `boneindex` |
| `0x08` | `name[64]` (путь компиляции) | | `0x94` | `numbonecontrollers` |
| `0x48` | ✔ `length` = размер файла | | `0x98` | `bonecontrollerindex` |
| `0x4C` | `eyeposition` | | `0x9C` | `numhitboxes` |
| `0x58` | `min` | | `0xA0` | `hitboxindex` |
| `0x64` | `max` | | `0xA4` | `numseq` |
| `0x70` | `bbmin` | | `0xA8` | ✔ `seqindex` (шаг `0xB0`) |
| `0x7C` | `bbmax` | | `0xAC` | `numseqgroups` |
| `0x88` | `flags` | | `0xB0` | `seqgroupindex` |
| | | | `0xB4` | `numtextures` |
| | | | `0xB8` | `textureindex` |
| | | | `0xBC` | `texturedataindex` |
| | | | `0xC0` | `numskinref` |
| | | | `0xC4` | `numskinfamilies` |
| | | | `0xC8` | `skinindex` |
| | | | `0xCC` | ✔ `numbodyparts` |
| | | | `0xD0` | `bodypartindex` |
| | | | `0xD4` | `numattachments` |
| | | | `0xD8` | `attachmentindex` |
| | | | `0xDC..0xF0` | sounds / transitions |

**Чем это отличается от HLSDK** (на этих отличиях легко обжечься):

* в заголовке **нет** `checksum`: после `version` сразу идёт `name[64]`, поэтому все
  последующие смещения сдвинуты на `-4` относительно HLSDK (`length` в `0x48`, а не `0x4C`);
* `version` в файлах — `10`, а не `48`/`0x30`. Загрузчик (`hw.dll+0x1006A23E`)
  после проверки `id == "IDST"` требует `length >= 0xF4`, а рядом есть
  `cmp dword [hdr+4], 0xB`; при этом все модели на диске имеют `10`, так что
  трактовать это как «версия >= 11» нельзя — сравнение относится к другому пути
  загрузки. Практический вывод: **на `version == 10` можно опираться**, а
  `>= 11` — нет.

Размеры записей (шаг массива проверен на моделях с 53 костями и 111 последовательностями):

| структура | размер | ключевые смещения |
|---|---|---|
| `mstudiobone_t` | `0x70` | `0x00` **name inline** `char[32]`, `0x20` ✔ parent, `0x24` flags, `0x28..0x40` `bonecontroller[6]`, `0x40..0x70` bind-pose (12 float) |
| `mstudioseqdesc_t` | `0xB0` | `0x00` name, `0x04` fps, `0x30` numframes, `0x70` numblends |
| `mstudiobodyparts_t` | `0x4C` | `0x00` name, `0x40` nummodels, `0x44` base, `0x48` modelindex |
| `mstudiomodel_t` | `0x70` | `0x00` name, `0x40` type, `0x44` radius, `0x48` nummesh |
| `mstudiotexture_t` | `0x50` | `0x00` name, `0x40` flags, `0x44` width, `0x48` height, `0x4C` index |
| `mstudiohitbox_t` | `0x20` | `0x00` bone, `0x04` group, `0x08` bbmin, `0x14` bbmax |
| `mstudioattachment_t` | **`0x58`** | `0x00` name, `0x20` bone, `0x24` org, `0x30..0x58` хвост (назначение не выяснено) |
| `mstudiobonecontroller_t` | `0x18` | `0x00` bone, `0x04` type, `0x08` start, `0x0C` end, `0x10` rest, `0x14` index (значения не проверялись, размер — по разметке массива) |

`mstudioattachment_t` в этом билде `0x58`, а не `0x18` из HLSDK: у `gign.mdl`
первый attachment начинается в `0x183C`, а `bonecontrollerindex` (`+0x98`) равен
`0x1824` при `numbonecontrollers = 1` — то есть `0x1824 + 0x18 = 0x183C` верно как
начало attachments, и следующий массив (`hitboxindex = 0x18EC`) отстоит от
`0x183C` на `2 * 0x58` при `numattachments = 2`.

Структуры продублированы в SDK (`golden-sdk::studio`) как `#[repr(C)]` с
`static_assert` на размер — если разметка разойдётся, это поймает компилятор.

---

## 8. Рецепты перехвата (retour-rs)

Правило проекта: все inline-хуки — через `retour` (`StaticDetour`/`RawDetour`), без самописных
трамплинов.

### 8.1 Подмена рендерера (самый чистый путь)

После того как клиент инициализировался (загружена карта), `hw.dll+0x271B04` содержит указатель на
`client.dll+0x1966B0`. Меняем его на свою `r_studio_interface_t`:

```rust
use std::ffi::c_void;

#[repr(C)]
pub struct RStudioInterface {
    pub version: i32,                                     // = 1
    pub draw_model: Option<unsafe extern "C" fn(i32) -> i32>,
    pub draw_player: Option<unsafe extern "C" fn(i32, *mut c_void) -> i32>,
}

type DrawModelFn = unsafe extern "C" fn(i32) -> i32;
type DrawPlayerFn = unsafe extern "C" fn(i32, *mut c_void) -> i32;

static mut ORIG_MODEL: Option<DrawModelFn> = None;
static mut ORIG_PLAYER: Option<DrawPlayerFn> = None;
static mut OUR: RStudioInterface = RStudioInterface { version: 1, draw_model: None, draw_player: None };

unsafe extern "C" fn draw_model(flags: i32) -> i32 {
    let r = ORIG_MODEL.map(|f| f(flags)).unwrap_or(0);
    overlay_after_draw();       // свой код: скелет/худ локального игрока и т.п.
    r
}

pub unsafe fn install(hw_base: usize) {
    let slot = (hw_base + 0x271B04) as *mut *mut RStudioInterface;
    let client_iface = *slot;                       // client.dll+0x1966B0
    ORIG_MODEL  = *((client_iface as *const usize).add(1) as *const DrawModelFn);
    ORIG_PLAYER = *((client_iface as *const usize).add(2) as *const DrawPlayerFn);
    OUR.draw_model  = Some(draw_model);
    OUR.draw_player = Some(ORIG_PLAYER.unwrap());
    *slot = &raw mut OUR;
}
```

Плюсы: не трогаем код игры, версия интерфейса (`1`) — единственное требование движка.
Минусы: `hw.dll` читает указатель при каждом кадре — подмену надо ставить после `Init()` и
восстанавливать при смене карты (проверять, что `*slot` всё ещё указывает на наш блок).

### 8.2 Доступ к матрицам костей после отрисовки

`m_pbonetransform` (`obj+0x40B4`) заполняется `StudioSetupBones` (`vtable+0x1C`) для сущности,
которую только что нарисовали. Проекция кости в экран:

```rust
#[repr(C)]
struct Vec3 { x: f32, y: f32, z: f32 }

unsafe fn bone_origin(obj: usize, i: usize) -> *const f32 {
    let arr = *(obj as *const usize).add(0x40B4 / 4) as *const f32; // m_pbonetransform
    arr.add(i * 12 + 3)                                            // трансляция: индексы 3, 7, 11
}
```

Матрица кости — `float[3][4]`, трансляция лежит в третьей колонке, то есть в плоском
массиве это индексы **3, 7, 11**. Порядок подтверждён декомпиляцией
`SV_StudioSetupBones` (`0x10092C70`): `fStack_244 * pfVar10[1] + *pfVar10 * x + y * pfVar10[2] + pfVar10[3]`
— последнее слагаемое и есть сдвиг. Индексы `9..11` (как в HLSDK-примерах, где матрица
записана как `float[3][4]` с транспонированием) дают мусор.

Дальше — обычная проекция (`WorldToScreen`, помним про FOV-соглашение 4:3 — см. заметку
про `tan_y = tan_x * 0.75`; готовая реализация — `golden_sdk::math`).

### 8.3 Хук на `R_StudioSetupModel` / `StudioSetupBones`

Если нужен полный контроль (своя отрисовка, а не оверлей поверх):

```rust
use retour::static_detour;

static_detour! { static HookSetupModel: unsafe extern "C" fn(i32, *mut *mut c_void, *mut *mut c_void); }

HookSetupModel.initialize(
    std::mem::transmute(hw_base + 0x10098E10),   // R_StudioSetupModel, cdecl
    |model, psub, pmodel| { /* можем подменить submodel/скин */ HookSetupModel.call(model, psub, pmodel) },
)?.enable()?;
```

Для `StudioSetupBones` (client `0x100ACB20`, thiscall) проще не inline-хук, а чтение полей
объекта `0x101F51C8` сразу после отрисовки — см. 8.2.

### 8.4 Чего в этом документе сознательно нет

Перехватов, которые раскрывают других игроков (ESP-скелеты, боксы по чужим моделям и т.п.). Всё
описанное рассчитано на модель локального игрока, вьюмодель, ботов в оффлайн-игре и инструменты
отладки/просмотра моделей.

---

## 9. Как проверить, что оффсеты не уехали

1. `hw.dll+0x271B04` после загрузки карты должен указывать на `client.dll+0x1966B0`; по этому
   адресу `[0] == 1`, `[1]` и `[2]` — указатели в диапазоне `client.dll+0x10000..+0x14C000`.
2. `hw.dll+0x271B08` — 48 указателей, все в `.text` `hw.dll` (`0x10001000..0x1014F000`).
3. В `client.dll`, объект `0x101F51C8`: `[0] == 0x10168F94`, `[0x4C/4]` — `studiohdr` текущей
   модели (проверять во время кадра отрисовки).
4. `studiohdr`: `id == 0x54534449` (`"IDST"`), `version == 10`, `0x8C` — `numbones` (не больше
   `0x80`), `0xA8` — `seqindex` (в пределах `length` из `0x48`).

Проверки 1–4 из этого списка автоматизированы: `golden_sdk::validate::run()`
(`cargo test` их не покрывает — нужен живой процесс игры, зато `golden-client`
пишет отчёт в `golden-client.log` при инжекте).

Если что-то не сходится — сборка другая, оффсеты надо перепроверить (см. раздел 10).

---

## 10. Что осталось неразобранным

- Имена слотов api `0x24, 0x2C, 0x34, 0x38, 0x40, 0x44, 0x4C, 0x64..0x74, 0x80, 0x94..0xAC, 0xB4, 0xB8`
  (поведение известно, HLSDK-имена — нет).
- Неназванные слоты vtable клиента (кроме 1–4, 7, 8, 10).
- Полный layout «текущей рендер-сущности» (`hw.dll+0x1078E470`): подтверждены поля `+0x2E4`
  (цвета), `+0x2FC`, `+0x304` (тип), `+0x308`, `+0x3C0`, `+0xB48` (origin), `+0xB60` (`player_info`),
  `+0xB94` (`model_t*`), `+0xBC0` (флаги); структура заметно больше классического `cl_entity_t`
  (`0x154`).
- `mstudioseqdesc_t`: подтверждён размер `0xB0`, но смещения `blend`/`animindex` стоит перепроверить
  по `0x100ACB20`.

---

## 11. Рисование скелета: что для этого нужно

Реверс отвечал на вопрос «какие адреса нужны, чтобы рисовать по костям». Итог — три вещи,
и все три уже есть в SDK:

1. **Координаты костей.** `CStudioModelRenderer::m_pbonetransform` (`+0x40B4`) — массив матриц
   `3x4` в **мировых** координатах, заполняется `StudioSetupBones` перед отрисовкой каждой модели.
   Матрица перезаписывается следующей моделью, поэтому читать её нужно сразу после оригинала
   `StudioDrawModel`, а не в HUD.
2. **Точка вклинивания.** Подмена `g_pStudioInterface` (`hw.dll+0x271B04`) на свою
   `r_studio_interface_t`: движок проверяет только `version == 1`. В своём `draw_model` сначала
   вызывается оригинал (он и посчитает кости), потом читаются матрицы.
3. **Чем рисовать.** `pTriAPI` для этого не нужен: `hw.dll` рисует через `opengl32.dll`, поэтому
   линии рисуются теми же `glBegin/glVertex3f/glEnd` из рендер-потока. Состояние сохраняется
   `glPushAttrib(GL_ALL_ATTRIB_BITS)`, так что кеш GL-состояния движка не ломается.

Важные следствия, которые сэкономили время:

- **Матрица вида.** Во время `StudioDrawModel` в GL активна только камера: позиция и углы
  сущности уже «впечены» в матрицы костей (`StudioSetupBones` берёт `currententity->origin`).
  Значит `glVertex3f` принимает мировые координаты напрямую, без `glTranslate`/`glRotate`.
- **Сортировка и глубина.** Рисование идёт в тот же проход, что и модель, тест глубины включён:
  скелет закрывается стенами. Для оверлея это и нужно (иначе получается wallhack).
- **Дефолтный интерфейс движка** (`hw.dll+0x271AF0`) костей не считает — до загрузки клиента в
  слоте лежит он, и `m_pbonetransform` пуст. Отсюда проверка `renderer::is_client_renderer_active()`.

### Раскладка кода

| Где | Что |
| --- | --- |
| `crates/golden-sdk/src/studio.rs` | формат `.mdl`, кости, рёбра скелета, матрицы |
| `crates/golden-sdk/src/renderer.rs` | объект рендерера, матрицы костей, подмена интерфейса |
| `crates/golden-sdk/src/draw.rs` | OpenGL: линии, цвет, сохранение состояния |
| `crates/golden-sdk/src/hook.rs` | `retour::RawDetour` и `SwappedInterface` |
| `crates/golden-sdk/src/player.rs` | `g_PlayerExtraInfo`: ник, команда, счёт, флаги |
| `crates/golden-std` | удобный слой: `Game`, `Player`, `Overlay` |
| `examples/esp` | сам оверлей: хук отрисовки + рисование |
| `examples/diagnostics` | проверка оффсетов в живой игре |

---

## 12. Живые смещения: игрок, оружие, команда

Эти адреса найдены не «на глаз», а по двум функциям `client.dll`, которые клиент
сам использует для таблицы счёта и для `StudioDrawPlayer`:

| Что | Где | Как найдено |
| --- | --- | --- |
| `g_PlayerExtraInfo` | `client.dll + 0x224E40` | база массива в `FUN_100BEC00` (сборка строк таблицы счёта: ключи `"name"`, `"frags"`, `"deaths"`, `"money"`, `"health"`, `"team"`) |
| шаг записи | `0xC0` | индексация `base + index * 0xC0` |
| чтение записи | `FUN_100BA530` | копирует поля записи в выходную структуру |

Поля записи `player_extra_info` (проверено в игре: значения совпадают с таблицей счёта):

| off | поле | off | поле |
|---|---|---|---|
| `0x00` | `char *name` (указатель на строку) | `0x30` | ✔ `has_c4` |
| `0x28` | ✔ `frags` | `0x31` | ✔ `vip` |
| `0x2C` | ✔ `deaths` | `0x32` | ✔ `dead` |
| `0x7C` | ✔ `teamnumber` (1 = T, 2 = CT) | `0x33` | ✔ `has_defusekit` |
| `0xB8` | ✔ `health` | `0xBC` | ✔ `money` |

Смещения в рендер-сущности (`cl_entity_t`, которую движок передаёт в
`StudioDrawPlayer`; проверено дампом и совпадением с моделью на экране):

| off | поле | примечание |
|---|---|---|
| `0x04` | `player` | признак «это игрок», всегда `1` — **не** номер: его легко принять за индекс |
| `0x10` | `origin` | позиция в мире |
| `0x184` | `model_t.model` → `studiohdr_t *` | заголовок только для чтения; `Mod_Extradata` здесь звать нельзя — при ошибке он делает `Host_Error` и убивает игру |
| `0x2E4` | `player_colors` | упакованные цвета модели |
| `0x368` | `weapon_model` | номер модели оружия в руках (индекс в массиве `model_t`) |

Номер игрока, которого движок рисует прямо сейчас, приходит в
`StudioDrawPlayer(int, player_info_s *)`: это `entity_state_t::number` — **второе**
поле структуры, `player_info + 0x04`. Он верен и для кастомных моделей, поэтому
именно он используется как ключ к `g_PlayerExtraInfo`, а не поиск по позиции.

Консольные команды: `Cbuf_AddText` = `hw.dll + 0x031E80` (найдено по строке
«Cbuf_AddText: buffer overflow»); команда уходит в буфер движка и выполняется
на ближайшем кадре, как из консоли. В SDK — `golden_sdk::engine::run_command`.

---

## 13. `golden-std`: удобный слой

`golden-sdk` намеренно низкоуровневый: смещения, чтение памяти, GL. Для кода
поверх него есть `crates/golden-std` — те же данные, но без `unsafe` и смещений:

```rust
use golden_std::Game;

let Some(game) = Game::get() else { return };
for name in game.get_players().map(|player| player.name()) {
    println!("{name}");
}
if let Some(local) = game.get_local_player() {
    println!("{} hp={}", local.name(), local.health());
}
```

| API | Что даёт |
| --- | --- |
| `Game::get()` | хендл игры текущего процесса |
| `game.get_players()` | итератор живых (нарисованных) игроков: ник, команда, HP, счёт, деньги, бомба, кит, оружие |
| `game.get_entities()` | все сущности с моделью |
| `game.get_local_player()` | локальный игрок (номер берётся из `GetTimes().view_entity`) |
| `game.run_command("+jump")` | консольная команда через `Cbuf_AddText` |
| `Overlay::new()` | камера кадра + FOV + вьюпорт |
| `overlay.skeleton(..)`, `overlay.head_circle(..)`, `overlay.label(..)` | отрисовка скелета, круга у головы и подписей |

Отрисовка по-прежнему требует вызова из хука отрисовки: там активен GL-контекст
и камера уже посчитана движком.

---

### Приложение: инструменты

Проверено статически: адреса, размеры структур, порядок вызовов. Проверка «на глаз» — только
инжектом в игру: SDK не может подтвердить, что `m_pbonetransform` действительно заполнен в
конкретный кадр, это видно лишь по нарисованным линиям.

---

### Приложение: инструменты

Реверс вёлся headless-Ghidra (12.2, JDK 25) со скриптами:

- `Dec.java` / `D4.java` — декомпиляция списка адресов в файл (по списку из `addrs*.txt`);
- `Mem.java` — дамп памяти по VA;
- `L.java` — дизассемблер по VA (для мелких стабов без `FUNCTION`);
- `hwx.py` — PE-обёртка (секции, RVA→offset, поиск строк/код-рефов);
- `api_scan.py` — поиск массивов указателей в `.rdata`/`.data`.

Всё это лежит вне репозитория (`%TEMP%\opencode\studio-re`), в проекте намеренно только результаты.