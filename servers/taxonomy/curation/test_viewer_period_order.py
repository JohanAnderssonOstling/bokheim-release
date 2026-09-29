"""Run with python3 -m unittest discover -s shared/subject-projection/tools -p test_viewer_period_order.py."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location('viewer', Path(__file__).with_name('generate-viewer.py'))
viewer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(viewer)


class PeriodOrderTests(unittest.TestCase):
    def test_dates(self):
        for label, expected in [
            ('19th Century', (1801, 1900)),
            ('16th–18th centuries', (1501, 1800)),
            ('19th-Century French', (1801, 1900)),
            ('Late 19th Century, 1865–1900', (1865, 1900)),
            ('333 BC–638 AD', (-333, 638)),
            ('500–100 BC', (-500, -100)),
            ('5th century BC', (-500, -400)),
            ('Early to 333 BC', (-100000, -333)),
            ('Early to 1500', (-100000, 1500)),
            ('Republic since 1918', (1918, 1918)),
            ('Early Modern, 1500–1815', (1500, 1815)),
        ]:
            with self.subTest(label=label):
                self.assertEqual(viewer.period_dates(label), expected)

    def test_live_period_menus(self):
        nodes = viewer.load_taxonomy()['nodes']
        self.assertEqual(nodes[3407]['c'], [3409, 3424, 3417, 3404, 3405, 3406])
        self.assertEqual(nodes[3618]['c'], [3480, 13690, 3666])
        self.assertEqual(nodes[11498]['c'], [3608, 10233, 10604, 10361, 10408])
        self.assertEqual(nodes[54295]['c'], [54296, 54297, 54298])
        self.assertEqual(nodes[54724]['c'], [11414, 11429, 11404, 11418, 11416, 11405])
        self.assertEqual(nodes[54754]['c'], viewer.PERIOD_MENU_ORDERS[54754])
        # Ordinary non-period menus retain their original alphabetical order.
        labels = [nodes[c]['l'].casefold() for c in nodes[3319]['c']]
        self.assertEqual(labels, sorted(labels))

    def test_scope_and_ties(self):
        nodes = {3319: {'l':'History','o':[]}, 1:{'l':'By Period','o':[]},
                 2:{'l':'Zulu, 1900–1910','o':[]}, 3:{'l':'Alpha, 1900–1910','o':[]},
                 4:{'l':'By Period','o':[]}}
        children = {3319:[1], 1:[2,3], 2:[], 3:[], 4:[2,3]}
        orders=viewer.history_period_orders(nodes,children)
        self.assertEqual(orders[1],[3,2])
        self.assertNotIn(4,orders)


if __name__ == '__main__':
    unittest.main()
