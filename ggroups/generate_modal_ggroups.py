# %%
from pathlib import Path
import json

json_file = Path(__file__).parent.parent / "python/nc_gcode_interpreter/ggroups.json"
with open(json_file, "r") as file:
    g_groups = json.load(file)
# %%

modal_ggroups = [
    group["short_name"] for group in g_groups if group["effectiveness"] == "modal"
]
non_modal_ggroups = [
    group["short_name"] for group in g_groups if group["effectiveness"] != "modal"
]


with open(Path(__file__).parent / "../src/MODAL_G_GROUPS.rs", "w") as file:
    file.write(f"pub const MODAL_G_GROUPS: [&str; {len(modal_ggroups)}] = [\n")
    for group in modal_ggroups:
        file.write(f'    "{group}",\n')
    file.write("];\n\n")
    file.write(f"pub const NON_MODAL_G_GROUPS: [&str; {len(non_modal_ggroups)}] = [\n")
    for group in non_modal_ggroups:
        file.write(f'    "{group}",\n')
    file.write("];\n")


# %%
