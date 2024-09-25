# %%
from pathlib import Path
import json

json_file = Path(__file__).parent / "ggroups.json"
with open(json_file, "r") as file:
    g_groups = json.load(file)
# %%

modal_ggroups = [
    group["short_name"] for group in g_groups if group["effectiveness"] == "modal"
]
modal_ggroups

with open(Path(__file__).parent / "../src/modal_groups.rs", "w") as file:
    file.write(f"pub const MODAL_GROUPS: [&str; {len(modal_ggroups)}] = [\n")
    for group in modal_ggroups:
        file.write(f'    "{group}",\n')
    file.write("];\n")

# %%
