# Third-party notices

EasyTier 2.6.4 is pinned at `8428a89d2dabc94c97d370ec607c6ca142473626`.
Its upstream LICENSE file contains the GNU Lesser General Public License version 3;
that notice is preserved in [licenses/EasyTier-LGPL-3.0.txt](licenses/EasyTier-LGPL-3.0.txt).
The incorporated GNU GPL version 3 text is preserved in
[licenses/GPL-3.0.txt](licenses/GPL-3.0.txt).
The upstream Cargo metadata says Apache-2.0; preserve the upstream license file
rather than replacing it with AVIAN's repository license.

The EasyTier patch files under `qualification/patches/` and the regression patch
under `qualification/routing-extension/` modify upstream source and retain its
applicable license terms. Engine builds record the matching upstream revision,
patch hashes, and binary hashes in `build.json`. Other dependencies retain their
own licenses. AVIAN's repository notice does not override third-party terms.
