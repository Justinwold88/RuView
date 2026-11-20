"""Blueprint 09 (companion fusion) running in a real Home Assistant core.

SYNTHETIC: the tests set entity states directly. They check the blueprint's
logic, not any sensor. Run from the repository root:

    python3.14 -m pip install -r examples/ha-blueprints/tests/requirements.txt
    cd examples/ha-blueprints/tests && python3.14 -m pytest -q
"""

from __future__ import annotations

import asyncio
import shutil
from datetime import timedelta
from pathlib import Path

from homeassistant.const import EVENT_HOMEASSISTANT_STARTED
from homeassistant.core import HomeAssistant
from homeassistant.setup import async_setup_component
from pytest_homeassistant_custom_component.common import async_fire_time_changed

BLUEPRINT = Path(__file__).resolve().parents[1] / "09-companion-fusion-occupancy.yaml"
HELPER = "input_boolean.room_occupied"
WIFI = "binary_sensor.ruview_presence"
WIFI_2 = "binary_sensor.ruview_presence_node_2"
MMWAVE = "binary_sensor.ld2410_presence"
BLE = "sensor.phone_area"
AUTOMATION = "automation.fusion"


async def set_up(
    hass: HomeAssistant, tmp_path: Path, helper_on: bool = False, **inputs
) -> None:
    """Load the blueprint from disk and create one automation from it."""
    hass.config.config_dir = str(tmp_path)
    folder = tmp_path / "blueprints" / "automation" / "ruvnet"
    folder.mkdir(parents=True)
    shutil.copy(BLUEPRINT, folder / BLUEPRINT.name)
    helper = {"room_occupied": {"initial": helper_on}}
    assert await async_setup_component(hass, "input_boolean", {"input_boolean": helper})
    use = {
        "path": f"ruvnet/{BLUEPRINT.name}",
        "input": {"occupancy_helper": HELPER, "wifi_presence": [WIFI], **inputs},
    }
    config = {"automation": {"id": "fusion", "alias": "fusion", "use_blueprint": use}}
    assert await async_setup_component(hass, "automation", config)
    await hass.async_block_till_done()
    automation = hass.states.get(AUTOMATION)
    assert automation is not None and automation.state == "on", automation


async def set_state(hass: HomeAssistant, entity_id: str, state: str) -> None:
    hass.states.async_set(entity_id, state)
    await hass.async_block_till_done()


async def settle(hass: HomeAssistant) -> None:
    """Let pending callbacks run without waiting for a paused automation run.

    `async_block_till_done` would wait for a run that is inside the restart
    branch's `wait_template`, which only ends when time moves on.
    """
    for _ in range(20):
        await asyncio.sleep(0)


async def advance(
    hass: HomeAssistant, freezer, seconds: float, block: bool = True
) -> None:
    freezer.tick(timedelta(seconds=seconds))
    async_fire_time_changed(hass)
    if block:
        await hass.async_block_till_done()
    else:
        await settle(hass)


async def fire_started(hass: HomeAssistant) -> None:
    hass.bus.async_fire(EVENT_HOMEASSISTANT_STARTED)
    await settle(hass)


def occupied(hass: HomeAssistant) -> bool:
    return hass.states.get(HELPER).state == "on"


async def test_wifi_alone_starts_and_the_clear_delay_ends(hass, tmp_path, freezer):
    await set_up(hass, tmp_path, clear_delay=60)
    await set_state(hass, WIFI, "off")
    assert not occupied(hass)

    await set_state(hass, WIFI, "on")
    assert occupied(hass), "a walk-in seen by Wi-Fi starts occupancy at once"

    await set_state(hass, WIFI, "off")
    await advance(hass, freezer, 59)
    assert occupied(hass), "clears only after the whole delay"
    await advance(hass, freezer, 2)
    assert not occupied(hass)


async def test_mmwave_holds_a_still_person_after_wifi_clears(hass, tmp_path, freezer):
    await set_up(hass, tmp_path, mmwave_presence=[MMWAVE], clear_delay=60)
    await set_state(hass, MMWAVE, "off")
    await set_state(hass, WIFI, "on")
    await set_state(hass, MMWAVE, "on")
    assert occupied(hass)

    await set_state(hass, WIFI, "off")
    await advance(hass, freezer, 600)
    assert occupied(hass), "mmWave keeps the room occupied while Wi-Fi is clear"

    await set_state(hass, MMWAVE, "off")
    await advance(hass, freezer, 61)
    assert not occupied(hass)


async def test_any_source_returning_restarts_the_clear_delay(hass, tmp_path, freezer):
    await set_up(hass, tmp_path, mmwave_presence=[MMWAVE], clear_delay=60)
    await set_state(hass, WIFI, "on")
    await set_state(hass, WIFI, "off")
    await set_state(hass, MMWAVE, "off")
    await advance(hass, freezer, 40)
    await set_state(hass, MMWAVE, "on")
    await set_state(hass, MMWAVE, "off")
    await advance(hass, freezer, 40)
    assert occupied(hass), "80 s after Wi-Fi cleared, but only 40 s after mmWave"
    await advance(hass, freezer, 21)
    assert not occupied(hass)


async def test_confirmation_mode_ignores_wifi_alone(hass, tmp_path, freezer):
    await set_up(
        hass,
        tmp_path,
        mmwave_presence=[MMWAVE],
        wifi_needs_confirmation=True,
        clear_delay=60,
    )
    await set_state(hass, MMWAVE, "off")
    await set_state(hass, WIFI, "on")
    await advance(hass, freezer, 30)
    assert not occupied(hass), "Wi-Fi alone must not start occupancy"

    await set_state(hass, MMWAVE, "on")
    assert occupied(hass), "mmWave confirms"

    await set_state(hass, MMWAVE, "off")
    await advance(hass, freezer, 600)
    assert occupied(hass), "Wi-Fi holds occupancy once it has started"

    await set_state(hass, WIFI, "off")
    await advance(hass, freezer, 61)
    assert not occupied(hass)

    await set_state(hass, WIFI, "on")
    assert not occupied(hass), "after clearing, Wi-Fi alone again needs confirmation"


async def test_ble_area_name_counts_only_for_this_room(hass, tmp_path, freezer):
    await set_up(
        hass,
        tmp_path,
        ble_presence=[BLE],
        ble_in_room_state="Living Room",
        wifi_needs_confirmation=True,
        clear_delay=60,
    )
    await set_state(hass, WIFI, "off")
    await set_state(hass, BLE, "Kitchen")
    assert not occupied(hass)

    await set_state(hass, BLE, "Living Room")
    assert occupied(hass)

    await set_state(hass, BLE, "Kitchen")
    await advance(hass, freezer, 61)
    assert not occupied(hass)


async def test_unavailable_sources_count_as_clear(hass, tmp_path, freezer):
    await set_up(hass, tmp_path, mmwave_presence=[MMWAVE], clear_delay=60)
    await set_state(hass, MMWAVE, "on")
    assert occupied(hass)

    await set_state(hass, MMWAVE, "unavailable")
    await set_state(hass, WIFI, "unavailable")
    await advance(hass, freezer, 61)
    assert not occupied(hass)


async def test_any_of_several_ruview_nodes_counts(hass, tmp_path, freezer):
    await set_up(hass, tmp_path, wifi_presence=[WIFI, WIFI_2], clear_delay=60)
    await set_state(hass, WIFI, "off")
    await set_state(hass, WIFI_2, "on")
    assert occupied(hass)
    await set_state(hass, WIFI_2, "off")
    await advance(hass, freezer, 61)
    assert not occupied(hass)


async def test_restart_clears_a_stale_helper_after_the_delay(hass, tmp_path, freezer):
    await set_up(
        hass, tmp_path, mmwave_presence=[MMWAVE], helper_on=True, clear_delay=60
    )
    await set_state(hass, WIFI, "off")
    await set_state(hass, MMWAVE, "off")
    assert occupied(hass), "restored as occupied, though everyone left"

    await fire_started(hass)
    await advance(hass, freezer, 59, block=False)
    assert occupied(hass)
    await advance(hass, freezer, 2)
    assert not occupied(hass)


async def test_restart_keeps_the_helper_when_someone_returns(hass, tmp_path, freezer):
    await set_up(
        hass, tmp_path, mmwave_presence=[MMWAVE], helper_on=True, clear_delay=60
    )
    await set_state(hass, WIFI, "unavailable")
    await set_state(hass, MMWAVE, "unavailable")

    await fire_started(hass)
    await advance(hass, freezer, 20, block=False)
    await set_state(hass, MMWAVE, "on")
    await advance(hass, freezer, 60)
    assert occupied(hass)


async def test_restart_turns_on_when_a_source_is_already_present(
    hass, tmp_path, freezer
):
    await set_up(hass, tmp_path, mmwave_presence=[MMWAVE], clear_delay=60)
    await set_state(hass, WIFI, "off")
    await set_state(hass, MMWAVE, "on")
    await hass.services.async_call(
        "input_boolean", "turn_off", {"entity_id": HELPER}, blocking=True
    )
    assert not occupied(hass)

    await fire_started(hass)
    await hass.async_block_till_done()
    assert occupied(hass)
