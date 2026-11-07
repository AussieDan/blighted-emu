// SPDX-License-Identifier: AGPL-3.0-or-later
//! The retail fast travel points of `newworld_vitaeeterna`: the spirit shrine, crow's nest, town
//! and outpost home points (kinds 11 and 7 of the home point list, 3652), as retail sends them.
//!
//! Observed (2026-11-06, every full home point list of 816 retail ledgers in `captures/` and
//! `open world captures/`): each point has one position and one cooldown in every ledger and for
//! every character, so the point is fixed game data, not where the player stood. The entity of
//! each one is the `capital_id` of its level capital (a `FastTravel_Teleporter*` slice, a crow's
//! nest slice, a settlement's `*_Settlement_Complete` slice or an outpost slice). Every shrine
//! teleporter, settlement and outpost capital of the level has a point here.
//!
//! - A shrine or crow's nest point lies on its shrine landmark (type 27, 0.0 m in 133 of 133); the
//!   rules find its chart achievement there ([`crate::travel_rules`]).
//! - A town or outpost point is added once the client says it came into the town's territory
//!   (2297): 41 of the 41 additions seen in the captures followed an entry into the territory
//!   named here (the settlement row, or the outpost row with its `OutpostId`); the territory holds
//!   the point (its landmark of type 1, 19 or 22).
//! - Left out: one shrine point of one capture (`select_world_with_unbound_char`, at 11358.7,
//!   2535.1) whose capital and landmark the level does not have.
//!
//! The values come from the probe `homescan` (tools repo, `scratch-work/capdecode`).

/// A retail shrine or crow's nest home point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetailFastTravelPoint {
    /// Its entity: the capital id of its level capital.
    pub entity: u64,
    /// Its position, metres.
    pub position: [f32; 3],
}

/// A retail town or outpost home point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetailTownPoint {
    /// The territory (`TerritoryID`) that gives it.
    pub territory: u16,
    /// Its entity: the capital id of the town's or outpost's level capital.
    pub entity: u64,
    /// Its position, metres.
    pub position: [f32; 3],
    /// Its cooldown, ns (30 s at five outposts, 0 elsewhere).
    pub cooldown_ns: u64,
}

/// The 133 shrine and crow's nest points, by entity.
pub const FAST_TRAVEL_POINTS: [RetailFastTravelPoint; 133] = [
    RetailFastTravelPoint {
        entity: 0x002c_8121_7ead_eebb,
        position: [10_259.0, 7_203.0, 447.937_16],
    },
    RetailFastTravelPoint {
        entity: 0x0314_7025_00e1_5153,
        position: [3_514.102, 6_414.019, 156.972_69],
    },
    RetailFastTravelPoint {
        entity: 0x087e_3b68_6044_cc03,
        position: [10_953.194, 6_773.421, 329.334_7],
    },
    RetailFastTravelPoint {
        entity: 0x0af7_5c56_8910_afeb,
        position: [4_206.716_3, 6_631.202_6, 277.284_97],
    },
    RetailFastTravelPoint {
        entity: 0x0d3f_88cd_2a97_aea3,
        position: [9_143.501, 6_654.813, 329.671_17],
    },
    RetailFastTravelPoint {
        entity: 0x0d51_447d_8bc1_77e3,
        position: [8_621.828, 2_579.765, 71.511_31],
    },
    RetailFastTravelPoint {
        entity: 0x0d62_b14d_3f38_609b,
        position: [12_098.147, 8_571.06, 305.464_6],
    },
    RetailFastTravelPoint {
        entity: 0x1011_9632_645f_35db,
        position: [10_744.148, 4_827.956, 96.745_77],
    },
    RetailFastTravelPoint {
        entity: 0x1054_68b0_22d1_ccc3,
        position: [9_603.31, 8_135.006, 434.916_87],
    },
    RetailFastTravelPoint {
        entity: 0x116b_ebb9_ba63_b32b,
        position: [10_225.770_5, 8_076.674, 602.229_7],
    },
    RetailFastTravelPoint {
        entity: 0x12f1_dcd2_8d19_b1bb,
        position: [3_733.261_7, 4_905.009, 99.376_46],
    },
    RetailFastTravelPoint {
        entity: 0x13fc_8ed2_5842_42b3,
        position: [7_501.459, 7_814.286, 202.790_79],
    },
    RetailFastTravelPoint {
        entity: 0x1441_d6af_4317_1c8b,
        position: [11_726.586, 4_181.414, 65.894_69],
    },
    RetailFastTravelPoint {
        entity: 0x169b_3329_5e93_48eb,
        position: [12_780.356, 8_328.779, 271.057_6],
    },
    RetailFastTravelPoint {
        entity: 0x19e1_eff6_ca22_d323,
        position: [6_687.073, 3_136.225_6, 45.980_713],
    },
    RetailFastTravelPoint {
        entity: 0x1bc9_d297_b6d2_2e63,
        position: [5_475.226, 6_187.922_4, 93.012_19],
    },
    RetailFastTravelPoint {
        entity: 0x1c53_5c7d_4c4e_eb63,
        position: [14_014.0, 7_832.0, 42.122_467],
    },
    RetailFastTravelPoint {
        entity: 0x1d86_1ced_2aaa_445b,
        position: [10_208.961, 5_895.822_8, 214.315_02],
    },
    RetailFastTravelPoint {
        entity: 0x20b7_cb73_6e44_6213,
        position: [4_516.076, 7_271.593, 163.982_57],
    },
    RetailFastTravelPoint {
        entity: 0x23f3_842e_3b18_419b,
        position: [9_657.9, 9_636.074, 646.538_4],
    },
    RetailFastTravelPoint {
        entity: 0x24ba_fee3_0c86_5933,
        position: [11_404.641, 3_094.312, 48.912_62],
    },
    RetailFastTravelPoint {
        entity: 0x2532_ed89_1f42_0153,
        position: [8_525.862, 4_540.009, 200.704_44],
    },
    RetailFastTravelPoint {
        entity: 0x2583_4328_83dc_5dfb,
        position: [11_917.529, 2_789.594_2, 69.582_75],
    },
    RetailFastTravelPoint {
        entity: 0x25a6_ee43_1761_134b,
        position: [9_340.737, 3_171.896_5, 94.561_29],
    },
    RetailFastTravelPoint {
        entity: 0x277c_4072_c9cc_e453,
        position: [11_896.117, 7_690.383, 219.479_06],
    },
    RetailFastTravelPoint {
        entity: 0x280c_e094_e686_ee0b,
        position: [8_809.25, 5_061.859, 171.528_56],
    },
    RetailFastTravelPoint {
        entity: 0x2899_e89d_f987_2d73,
        position: [6_525.118_7, 4_548.878, 196.626_66],
    },
    RetailFastTravelPoint {
        entity: 0x2902_1503_d1d3_c28b,
        position: [8_100.876_5, 599.947_7, 41.122_463],
    },
    RetailFastTravelPoint {
        entity: 0x2a6e_e796_a52c_43a3,
        position: [11_868.143, 8_533.395, 316.145_6],
    },
    RetailFastTravelPoint {
        entity: 0x2b5a_991e_7c35_e653,
        position: [9_343.633, 9_268.537, 548.122_5],
    },
    RetailFastTravelPoint {
        entity: 0x2ca5_ee7c_0d84_c53b,
        position: [4_467.076, 6_753.554, 144.122_47],
    },
    RetailFastTravelPoint {
        entity: 0x2d29_1d88_bcc7_2c13,
        position: [5_005.833, 5_478.121, 107.351_05],
    },
    RetailFastTravelPoint {
        entity: 0x31aa_427e_1945_2f63,
        position: [9_338.197, 8_090.280_3, 437.965_7],
    },
    RetailFastTravelPoint {
        entity: 0x3358_c9d2_6df9_2fb3,
        position: [9_262.989, 4_744.023, 136.058_47],
    },
    RetailFastTravelPoint {
        entity: 0x3472_2ea1_6549_423b,
        position: [8_566.953, 1_524.719_7, 235.297_93],
    },
    RetailFastTravelPoint {
        entity: 0x353d_621c_c3ae_d303,
        position: [2_536.353, 6_430.025_4, 113.786_995],
    },
    RetailFastTravelPoint {
        entity: 0x3846_2364_6deb_2bfb,
        position: [10_790.28, 3_791.091_6, 117.659_18],
    },
    RetailFastTravelPoint {
        entity: 0x38b2_fbaf_e020_8a43,
        position: [9_643.337, 7_570.235_4, 327.272_2],
    },
    RetailFastTravelPoint {
        entity: 0x3f06_302a_18b1_ebcb,
        position: [10_033.116, 3_195.782_2, 173.378_16],
    },
    RetailFastTravelPoint {
        entity: 0x3f70_c427_42b7_28f3,
        position: [8_520.667, 1_244.499, 231.618_04],
    },
    RetailFastTravelPoint {
        entity: 0x4821_9718_6d17_f7cb,
        position: [7_570.099, 6_595.36, 285.497_68],
    },
    RetailFastTravelPoint {
        entity: 0x4947_3ff8_a029_e033,
        position: [7_471.647, 8_385.194, 193.784_59],
    },
    RetailFastTravelPoint {
        entity: 0x4a5c_2b62_491a_6223,
        position: [7_243.876, 376.264_43, 43.695_267],
    },
    RetailFastTravelPoint {
        entity: 0x4ccb_b4f0_469e_dbeb,
        position: [6_506.149_4, 7_604.533_7, 111.542_305],
    },
    RetailFastTravelPoint {
        entity: 0x4e62_164d_73e9_0a8b,
        position: [11_957.383, 3_267.463, 78.505_06],
    },
    RetailFastTravelPoint {
        entity: 0x5173_5759_9785_db9b,
        position: [7_684.655, 3_035.277_8, 104.920_99],
    },
    RetailFastTravelPoint {
        entity: 0x529d_3c6d_48c6_093b,
        position: [9_749.132, 388.839_75, 56.108_77],
    },
    RetailFastTravelPoint {
        entity: 0x5404_0baf_565d_eb13,
        position: [6_522.239, 5_258.654_3, 75.984_23],
    },
    RetailFastTravelPoint {
        entity: 0x54c0_1648_61d3_6e03,
        position: [10_966.786, 8_258.837, 156.694_7],
    },
    RetailFastTravelPoint {
        entity: 0x5507_7f49_62e7_806b,
        position: [9_353.803, 4_002.621_8, 101.688_644],
    },
    RetailFastTravelPoint {
        entity: 0x56ff_f5b5_e566_90a3,
        position: [8_431.604_5, 5_711.232_4, 122.518_9],
    },
    RetailFastTravelPoint {
        entity: 0x5763_8bc3_75b6_84f3,
        position: [8_160.505_4, 5_349.96, 52.209_423],
    },
    RetailFastTravelPoint {
        entity: 0x57f8_904b_5540_61ab,
        position: [6_451.639, 6_287.133, 46.809_85],
    },
    RetailFastTravelPoint {
        entity: 0x58a3_d74f_3ac2_c49b,
        position: [7_295.964, 9_150.696, 246.883_91],
    },
    RetailFastTravelPoint {
        entity: 0x58f7_8906_2dc4_c26b,
        position: [2_974.164_8, 7_017.416_5, 98.689_74],
    },
    RetailFastTravelPoint {
        entity: 0x59b8_9ba8_4b92_2943,
        position: [10_697.668, 7_510.114_7, 304.479_74],
    },
    RetailFastTravelPoint {
        entity: 0x5c4a_1eb2_eb93_588b,
        position: [8_012.868_7, 7_687.119, 519.056_76],
    },
    RetailFastTravelPoint {
        entity: 0x5f36_93a8_2695_a613,
        position: [3_991.320_6, 5_604.595_7, 119.365_395],
    },
    RetailFastTravelPoint {
        entity: 0x6156_1c30_597a_0a23,
        position: [2_363.496, 7_055.485, 121.952_286],
    },
    RetailFastTravelPoint {
        entity: 0x65e6_b4bf_3ad1_a613,
        position: [7_315.68, 4_660.387, 42.543_617],
    },
    RetailFastTravelPoint {
        entity: 0x66f7_9b2b_68e8_613b,
        position: [7_132.861, 1_578.325_3, 41.674_87],
    },
    RetailFastTravelPoint {
        entity: 0x6932_aada_3c0b_117b,
        position: [9_829.289, 3_019.452, 80.481_96],
    },
    RetailFastTravelPoint {
        entity: 0x6ae1_6c59_0b7f_2b5b,
        position: [13_474.061, 5_318.299, 79.052_59],
    },
    RetailFastTravelPoint {
        entity: 0x6bc8_391e_7908_85a3,
        position: [6_437.623_5, 2_876.330_3, 48.429_726],
    },
    RetailFastTravelPoint {
        entity: 0x6be4_dfab_458c_f5d3,
        position: [8_792.469, 8_467.964, 632.341_8],
    },
    RetailFastTravelPoint {
        entity: 0x720b_50ea_dc78_c443,
        position: [11_472.144, 6_715.149, 484.782_3],
    },
    RetailFastTravelPoint {
        entity: 0x733a_8578_e132_589b,
        position: [12_200.346, 7_251.305_7, 231.136_15],
    },
    RetailFastTravelPoint {
        entity: 0x7521_d178_1270_8d0b,
        position: [8_950.035, 7_053.505_4, 408.691_35],
    },
    RetailFastTravelPoint {
        entity: 0x76c0_8bae_d1cf_0b5b,
        position: [9_560.338, 4_204.205, 108.546_17],
    },
    RetailFastTravelPoint {
        entity: 0x775c_37f3_f676_2aab,
        position: [8_044.971, 2_173.630_1, 66.511_89],
    },
    RetailFastTravelPoint {
        entity: 0x7b22_cbdc_da9f_507b,
        position: [9_267.81, 1_792.361_2, 58.356_625],
    },
    RetailFastTravelPoint {
        entity: 0x7bdf_2e0e_0775_f40b,
        position: [9_378.29, 7_193.620_6, 303.421_75],
    },
    RetailFastTravelPoint {
        entity: 0x7be6_63af_1d68_76e3,
        position: [2_879.136, 5_937.474_6, 73.696_14],
    },
    RetailFastTravelPoint {
        entity: 0x7d31_893a_4b49_1fa3,
        position: [5_639.826_7, 7_734.264_6, 94.581_635],
    },
    RetailFastTravelPoint {
        entity: 0x833a_9d1c_c2af_5843,
        position: [7_976.490_7, 6_211.835_4, 188.200_29],
    },
    RetailFastTravelPoint {
        entity: 0x8361_99f9_19fc_0f73,
        position: [5_365.05, 6_706.644, 250.880_57],
    },
    RetailFastTravelPoint {
        entity: 0x839f_3950_ecaf_b763,
        position: [5_075.066_4, 8_580.774, 181.495_39],
    },
    RetailFastTravelPoint {
        entity: 0x8419_8e50_ceda_7f9b,
        position: [7_071.763_7, 669.481_4, 90.573_14],
    },
    RetailFastTravelPoint {
        entity: 0x85dc_5341_ec43_bebb,
        position: [8_938.02, 1_410.356_2, 57.410_736],
    },
    RetailFastTravelPoint {
        entity: 0x888d_22b7_b7f0_4d13,
        position: [5_011.727, 6_029.148, 146.097_17],
    },
    RetailFastTravelPoint {
        entity: 0x88fa_8bfc_c2e9_373b,
        position: [8_794.684, 6_174.976, 244.121_32],
    },
    RetailFastTravelPoint {
        entity: 0x89a3_4955_7d13_a4db,
        position: [5_558.786_6, 4_309.561, 42.533_5],
    },
    RetailFastTravelPoint {
        entity: 0x8a8e_fa6a_c418_12a3,
        position: [7_145.405_3, 8_155.444, 186.708_28],
    },
    RetailFastTravelPoint {
        entity: 0x8b02_1727_c650_bde3,
        position: [5_046.186, 9_513.642, 319.999_66],
    },
    RetailFastTravelPoint {
        entity: 0x926c_c394_0691_b373,
        position: [9_889.486, 5_186.293, 169.209_44],
    },
    RetailFastTravelPoint {
        entity: 0x990c_821c_dbf2_47d3,
        position: [11_504.085, 8_246.713, 201.719_18],
    },
    RetailFastTravelPoint {
        entity: 0x9f03_4493_0188_f93b,
        position: [3_970.644_3, 7_831.905_3, 140.599_76],
    },
    RetailFastTravelPoint {
        entity: 0xa1e1_866d_bc85_e11b,
        position: [7_536.468_3, 3_985.929_7, 118.270_46],
    },
    RetailFastTravelPoint {
        entity: 0xa24d_1c30_8978_d623,
        position: [10_535.84, 5_501.202_6, 121.943_59],
    },
    RetailFastTravelPoint {
        entity: 0xa323_a31d_da0a_2573,
        position: [9_908.573, 4_785.814_5, 122.391_464],
    },
    RetailFastTravelPoint {
        entity: 0xaaca_10ed_35eb_57fb,
        position: [6_032.805, 9_047.859, 185.426_22],
    },
    RetailFastTravelPoint {
        entity: 0xac72_4cb1_3c6b_4bb3,
        position: [4_342.948_7, 5_324.981_4, 101.321_46],
    },
    RetailFastTravelPoint {
        entity: 0xaefa_6e4e_5944_80c3,
        position: [4_027.519, 7_423.645_5, 144.424_58],
    },
    RetailFastTravelPoint {
        entity: 0xaf7e_17c6_5cfe_bb5b,
        position: [8_329.948, 9_451.442, 607.760_4],
    },
    RetailFastTravelPoint {
        entity: 0xb0ce_e920_c034_be0b,
        position: [4_274.275_4, 9_321.501, 372.329_3],
    },
    RetailFastTravelPoint {
        entity: 0xb334_4aad_196a_e38b,
        position: [7_005.803, 6_164.263, 74.630_94],
    },
    RetailFastTravelPoint {
        entity: 0xb3d3_959f_ebc4_3a93,
        position: [5_289.208, 8_326.549, 182.878_77],
    },
    RetailFastTravelPoint {
        entity: 0xbadc_c4e8_e2c8_b143,
        position: [10_196.582, 7_733.983_4, 441.635],
    },
    RetailFastTravelPoint {
        entity: 0xc3f7_98eb_3ea7_6ce3,
        position: [11_583.187, 6_973.829_6, 324.661_53],
    },
    RetailFastTravelPoint {
        entity: 0xc765_4da0_6992_3b2b,
        position: [6_062.417_5, 9_854.083, 323.621_98],
    },
    RetailFastTravelPoint {
        entity: 0xc7c3_6335_1729_ecab,
        position: [9_177.921, 455.916_93, 70.863_18],
    },
    RetailFastTravelPoint {
        entity: 0xca2c_5142_183f_99ab,
        position: [8_747.144, 7_729.657, 637.122_5],
    },
    RetailFastTravelPoint {
        entity: 0xcb2b_2c51_5f3b_386b,
        position: [6_861.488, 842.283_2, 49.732_098],
    },
    RetailFastTravelPoint {
        entity: 0xcccf_75c4_8cdc_2bc3,
        position: [11_456.308, 6_235.269, 144.482_42],
    },
    RetailFastTravelPoint {
        entity: 0xcde4_7871_83ea_dc23,
        position: [6_615.183_6, 1_197.860_2, 42.121_586],
    },
    RetailFastTravelPoint {
        entity: 0xd256_86df_6795_f86b,
        position: [7_131.725, 2_392.561_5, 80.686_615],
    },
    RetailFastTravelPoint {
        entity: 0xd59b_a380_3700_7c5b,
        position: [11_078.555, 3_662.536, 148.914_2],
    },
    RetailFastTravelPoint {
        entity: 0xd788_a911_00a1_bf23,
        position: [9_716.081, 1_881.700_9, 43.550_266],
    },
    RetailFastTravelPoint {
        entity: 0xd7bb_1162_e378_484b,
        position: [11_957.117, 5_696.25, 47.498_627],
    },
    RetailFastTravelPoint {
        entity: 0xda48_f548_eca9_a2cb,
        position: [3_119.390_9, 8_054.404, 189.703_74],
    },
    RetailFastTravelPoint {
        entity: 0xda76_fc2d_6ed8_8d73,
        position: [12_947.0, 5_552.0, 85.122_47],
    },
    RetailFastTravelPoint {
        entity: 0xdb2f_57f0_193e_de4b,
        position: [9_868.836, 7_894.434, 453.626_22],
    },
    RetailFastTravelPoint {
        entity: 0xdca0_7260_de2f_505b,
        position: [11_208.114, 5_110.85, 42.261_25],
    },
    RetailFastTravelPoint {
        entity: 0xdd60_eb93_8ab3_08db,
        position: [6_982.995_6, 4_112.415_5, 50.168_213],
    },
    RetailFastTravelPoint {
        entity: 0xdd90_d300_2595_4653,
        position: [2_605.631, 4_551.477_5, 58.684_273],
    },
    RetailFastTravelPoint {
        entity: 0xe0de_199e_7344_d6e3,
        position: [4_213.4, 6_101.888, 110.073_2],
    },
    RetailFastTravelPoint {
        entity: 0xe123_a07b_2cb5_dfd3,
        position: [11_094.98, 8_588.271, 246.610_73],
    },
    RetailFastTravelPoint {
        entity: 0xe30a_379c_4730_4963,
        position: [2_468.887, 5_155.137_7, 48.670_513],
    },
    RetailFastTravelPoint {
        entity: 0xe59a_81bd_000b_2103,
        position: [5_076.682_6, 7_704.043_5, 194.547_55],
    },
    RetailFastTravelPoint {
        entity: 0xe7a5_3e88_5c2e_c563,
        position: [9_406.142, 5_765.843, 190.429_57],
    },
    RetailFastTravelPoint {
        entity: 0xe7b0_d46c_fee3_25ab,
        position: [9_042.658, 2_241.483_4, 133.186_28],
    },
    RetailFastTravelPoint {
        entity: 0xeb2e_f28b_8d01_679b,
        position: [7_125.14, 6_922.491_7, 188.728_26],
    },
    RetailFastTravelPoint {
        entity: 0xef7b_a74e_beff_c51b,
        position: [10_013.471, 4_122.296, 142.436_69],
    },
    RetailFastTravelPoint {
        entity: 0xf5a9_0c42_653b_1c8b,
        position: [3_427.324_7, 7_753.312_5, 103.373_29],
    },
    RetailFastTravelPoint {
        entity: 0xf723_065c_4226_8773,
        position: [8_746.213, 9_896.63, 546.032],
    },
    RetailFastTravelPoint {
        entity: 0xf890_984f_bcfe_574b,
        position: [8_787.934, 765.358_4, 75.622_47],
    },
    RetailFastTravelPoint {
        entity: 0xf8ed_46e9_6d84_aaa3,
        position: [10_493.163, 2_662.027_6, 91.569_084],
    },
    RetailFastTravelPoint {
        entity: 0xf9c6_2b8f_6ebc_df83,
        position: [8_202.168, 3_462.169_4, 107.908_63],
    },
    RetailFastTravelPoint {
        entity: 0xfa47_4e40_a6f4_9f43,
        position: [7_880.268, 3_688.787_6, 176.040_02],
    },
    RetailFastTravelPoint {
        entity: 0xfe5c_df1c_140c_bbab,
        position: [11_774.014, 3_555.448_7, 69.930_22],
    },
    RetailFastTravelPoint {
        entity: 0xfeae_dd6e_8534_ca2b,
        position: [3_310.476, 6_824.857_4, 121.916_84],
    },
    RetailFastTravelPoint {
        entity: 0xfec6_d0b9_f1c2_822b,
        position: [7_354.713_4, 1_186.845_7, 47.153_793],
    },
    RetailFastTravelPoint {
        entity: 0xfef0_dff0_09af_32c3,
        position: [11_131.049, 7_304.652_3, 169.381_6],
    },
];

/// The 22 town and outpost points, by territory.
pub const TOWN_POINTS: [RetailTownPoint; 22] = [
    RetailTownPoint {
        territory: 10_330,
        entity: 0xca87_2187_ea0c_f03b,
        position: [3_878.831_3, 6_982.706_5, 157.289_55],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 20_333,
        entity: 0x104f_93e4_0eff_6293,
        position: [4_778.406_3, 6_801.678_7, 158.451_55],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 20_426,
        entity: 0x2721_6c6a_7554_0c9b,
        position: [4_496.849, 8_682.276, 284.520_7],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 30_044,
        entity: 0x58ec_13ff_6e93_107b,
        position: [7_859.614_7, 1_951.507_3, 112.902_504],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 30_155,
        entity: 0xe0f2_6061_7543_bb93,
        position: [7_359.36, 3_792.735, 117.079_765],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 30_253,
        entity: 0x4f43_c63c_9b90_25b3,
        position: [7_250.163, 5_434.9, 113.341_43],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 30_326,
        entity: 0x5ff3_d953_ec90_bf6b,
        position: [7_757.271, 7_229.274_4, 187.986_6],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 30_434,
        entity: 0x774c_e5da_f7ab_3193,
        position: [6_338.603, 8_388.133, 169.0],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 40_062,
        entity: 0x2f4a_1a62_63eb_a8eb,
        position: [8_716.41, 1_976.645_4, 127.275_88],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 40_066,
        entity: 0x6564_f095_c202_f3d3,
        position: [9_808.0, 1_159.0, 73.751_94],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 40_186,
        entity: 0x8188_eb58_aa60_b52b,
        position: [9_342.281, 2_664.482, 82.661_96],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 40_300,
        entity: 0x82c5_6d44_2ca1_2c03,
        position: [8_976.994, 4_215.463, 150.057_7],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 40_324,
        entity: 0xf10b_b4d5_c057_24eb,
        position: [8_821.395, 7_926.343_8, 544.257_6],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 40_325,
        entity: 0xb464_9264_871a_b103,
        position: [10_066.262, 7_538.833_5, 414.867_55],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 40_352,
        entity: 0xe2b9_b9a5_b802_371b,
        position: [9_664.770_5, 6_355.684_6, 252.186_77],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 40_406,
        entity: 0x3cb2_f251_16d4_ba5b,
        position: [9_248.369, 9_066.466, 617.107_24],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 50_155,
        entity: 0x1332_d93c_fb7b_b29b,
        position: [10_892.04, 3_312.031_5, 76.159_676],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 50_257,
        entity: 0xe4ef_ed02_7812_399b,
        position: [11_459.452, 5_394.013_7, 41.765_717],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 50_401,
        entity: 0xefbd_58ea_8469_0ceb,
        position: [10_299.799, 8_900.838, 712.892_94],
        cooldown_ns: THIRTY_SECONDS_NS,
    },
    RetailTownPoint {
        territory: 53_010,
        entity: 0x3be6_aead_9be0_caeb,
        position: [10_933.692, 7_783.602, 155.854_48],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 60_241,
        entity: 0xc084_2df7_3fe7_2fab,
        position: [13_079.562, 4_423.526_4, 76.162_26],
        cooldown_ns: 0,
    },
    RetailTownPoint {
        territory: 60_353,
        entity: 0x6866_f41e_00ce_e563,
        position: [13_226.956, 6_960.606_4, 149.067_8],
        cooldown_ns: 0,
    },
];

/// A retail inn home point (kind 6): its name and position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetailInnPoint {
    /// Its name text (`@<settlement>_InnName`, with `_1` or `_2` where a settlement has two).
    pub name: &'static str,
    /// Its position, metres: the inn's `InnInteract` landmark (type 21), where the innkeeper
    /// capital the player binds to stands.
    pub position: [f32; 3],
}

/// Every distinct inn home point of the retail home point lists (3652) of 1,085 ledgers of
/// `captures/` (probe `innpoint`, 2026-11-07). Retail names each; the name cannot be derived:
/// settlements 3, 9 and 16 have two inns, and five points name a quest giver.
pub const INN_POINTS: [RetailInnPoint; 23] = [
    RetailInnPoint {
        name: "@02_InnName",
        position: [9_585.529, 6_312.614, 252.917_72],
    },
    RetailInnPoint {
        name: "@03_InnName_1",
        position: [8_852.265, 7_962.464_4, 545.888_8],
    },
    RetailInnPoint {
        name: "@03_InnName_2",
        position: [10_072.505, 7_510.389, 416.354_98],
    },
    RetailInnPoint {
        name: "@04_InnName",
        position: [8_910.835, 4_207.41, 140.254_03],
    },
    RetailInnPoint {
        name: "@05_InnName",
        position: [10_945.23, 3_308.091_6, 88.918_64],
    },
    RetailInnPoint {
        name: "@06_InnName",
        position: [9_319.677, 2_667.698_2, 84.370_98],
    },
    RetailInnPoint {
        name: "@07_InnName_1",
        position: [9_222.297, 9_048.451, 618.619_2],
    },
    RetailInnPoint {
        name: "@08_InnName",
        position: [7_290.615, 5_335.991_7, 88.156_715],
    },
    RetailInnPoint {
        name: "@09_InnName_1",
        position: [8_712.5, 1_975.5, 127.6],
    },
    RetailInnPoint {
        name: "@09_InnName_2",
        position: [9_821.145_5, 1_158.787_1, 69.181_13],
    },
    RetailInnPoint {
        name: "@10_InnName",
        position: [7_890.327, 1_941.200_7, 112.116_005],
    },
    RetailInnPoint {
        name: "@11_InnName",
        position: [13_186.802, 6_930.315_4, 150.740_43],
    },
    RetailInnPoint {
        name: "@12_InnName",
        position: [7_383.5, 3_732.5, 112.669],
    },
    RetailInnPoint {
        name: "@13_InnName",
        position: [11_443.54, 5_271.1, 45.284_393],
    },
    RetailInnPoint {
        name: "@14_InnName_1",
        position: [10_896.13, 7_875.032, 172.159_68],
    },
    RetailInnPoint {
        name: "@15_InnName",
        position: [12_968.001, 4_392.619, 68.405_17],
    },
    RetailInnPoint {
        name: "@16_InnName_1",
        position: [6_359.453_6, 8_349.1, 169.75],
    },
    RetailInnPoint {
        name: "@16_InnName_2",
        position: [7_729.359, 7_215.601_6, 185.212_25],
    },
    RetailInnPoint {
        name: "@9401_Horse",
        position: [8_953.742, 5_227.418_5, 150.884_67],
    },
    RetailInnPoint {
        name: "@9402_Conman",
        position: [10_700.0, 4_814.0, 102.861_32],
    },
    RetailInnPoint {
        name: "@9404_Record",
        position: [4_975.0, 8_784.0, 185.637_01],
    },
    RetailInnPoint {
        name: "@NPC_20008_Name",
        position: [4_779.003, 6_798.184, 158.050_77],
    },
    RetailInnPoint {
        name: "@NPC_28018_Name",
        position: [3_801.722_2, 6_936.718_8, 155.357_39],
    },
];

/// How far apart a level position and a retail inn point may be on each axis and still be the
/// same point, m (the retail values are the level's `f32` values).
const SAME_POINT_M: f32 = 0.01;

/// The retail name of the inn home point at `position`; `None` for a position no retail inn
/// point has.
pub fn inn_name(position: [f32; 3]) -> Option<&'static str> {
    INN_POINTS
        .iter()
        .find(|inn| {
            inn.position
                .iter()
                .zip(position)
                .all(|(retail, level)| (retail - level).abs() <= SAME_POINT_M)
        })
        .map(|inn| inn.name)
}

/// The cooldown of an outpost point that has one (Observed: 30 000 000 000 ns).
const THIRTY_SECONDS_NS: u64 = 30_000_000_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inn_point_has_its_retail_name_and_another_place_none() {
        // The Prydwen inn (settlement 12): the innkeeper capital the live player bound to.
        const PRYDWEN_INN: [f32; 3] = [7_383.5, 3_732.5, 112.669];
        assert_eq!(inn_name(PRYDWEN_INN), Some("@12_InnName"));
        assert_eq!(
            inn_name([PRYDWEN_INN[0] + 1.0, PRYDWEN_INN[1], PRYDWEN_INN[2]]),
            None
        );
        let names: std::collections::HashSet<&str> =
            INN_POINTS.iter().map(|inn| inn.name).collect();
        assert_eq!(names.len(), INN_POINTS.len(), "one point per name");
    }
}
