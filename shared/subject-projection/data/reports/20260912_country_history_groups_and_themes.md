# Country history navigation and thematic assignments — 2026-09-12

Country history subjects receive general histories directly. Military and naval history share one substantive child; political history and foreign relations share another. Period navigation is uncoded. Geographic navigation is merged into an uncoded Local History subject with named places and a coded Other Local History fallback.

Existing bilateral relationship subject IDs, labels and codes are preserved. Their navigation remains within the political/foreign-relations branch. Country-specific diplomatic A–Z ranges remain assigned there even when no individual-country subjects have yet been curated.

## Scope and evidence

Reviewed 162 existing country and territory branches (including historical and nested country scopes). Created 814 subjects; consolidated 53 older subjects. No new country roots were invented.

Source: the repository’s updated LCC reference database, classification records and caption hierarchies, checked against the official 2025 DS–DX schedule text for missing or malformed machine-readable headings. The modern country scopes in the taxonomy are not a one-to-one list of sovereign states.

- [Official D–DR schedule](https://www.loc.gov/aba/publications/FreeLCC/LCC_D-DR2025TEXT.pdf)
- [Official DS–DX schedule](https://www.loc.gov/aba/publications/FreeLCC/LCC_DS-DX2025TEXT.pdf)

The existing reference import omits some unnumbered period and local headings. Myanmar, Cambodia, Laos, Vietnam, Nepal, Bhutan and Sri Lanka use reviewed schedule-page boundaries instead. Named places retain narrower assignments; Other Local History owns broad fallback ranges.

## Validation

- Migration replay produces exactly the reviewed database tables.
- SQLite integrity and foreign-key checks pass; hierarchy cycle check passes.
- Every resulting By Period and Local History grouping has zero LCC, DDC and BISAC assignments.
- All Other Local History subjects have code assignments.
- All 39 existing bilateral relationship subjects retain IDs, labels and code assignments.
- All 270 existing named local subjects placed under Local History retain IDs and code assignments.
- 11,977,153 complete cached notations compared; 252,309 changed destinations; 0 lost matches; 1 newly matched notation. Final fallback corrections checked against all 4,228,536 LCC strings containing D, E or F; all other codes are unaffected by those LCC-only selector changes. No existing named-place matches were displaced by Other Local History.
- All 16 focused routing examples pass, including Danish bilateral relations, Stockholm, other Swedish towns, general history and period subjects.

## Reviewed branches

| ID | Country / territory branch | Military & naval | Political & foreign relations | By Period | Local History |
|---|---|---|---|---|---|
| 3486 | Afghan History | 54626 | 54627 | 54628 | 54629 |
| 3544 | Albanian History | 54675 | 51829 | 54676 | 14008 |
| 3321 | Algerian History | — | 54266 | 54267 | 54276 |
| 3554 | Andorran History | — | — | 54689 | 51998 |
| 3322 | Angolan History | — | 54530 | 54531 | 54537 |
| 3634 | Arabian Peninsula & Saudi Arabia | — | 54712 | 54713 | 54714 |
| 10499 | Argentine History | 54499 | 54500 | 54501 | 54504 |
| 10717 | Armenian History | — | — | 54518 | 54519 |
| 3488 | Armenian History | — | 54631 | 54632 | — |
| 3677 | Australian History | 54737 | 54738 | 54739 | 54740 |
| 3537 | Austrian History | 54803 | 54804 | 54805 | 13761 |
| 3586 | Azerbaijani History | 54447 | 54448 | 54449 | 13988 |
| 3386 | Bahamas | — | — | 54358 | 54360 |
| 3490 | Bangladesh. East Pakistan | — | 54633 | 54634 | 54635 |
| 3587 | Belarusian History | 51896 | 51897 | 54451 | 54452 |
| 3577 | Belgian History | 54818 | 51790 | 54819 | 13751 |
| 3387 | Belizean History | — | 54362 | 54363 | 54364 |
| 13768 | Beninese History | — | 54173 | 54174 | 54178 |
| 3491 | Bhutanese History | — | 54763 | 54764 | 54767 |
| 3426 | Bolivian History | 54396 | 54397 | 54398 | 54399 |
| 51957 | Bosnia and Herzegovina History | 54259 | 54260 | 54261 | 51968 |
| 3324 | Botswana. Bechuanaland | — | 54539 | 54540 | 54544 |
| 3427 | Brazilian History | 54401 | 54402 | 54403 | 50873 |
| 3521 | British Isles History | — | 51030 | 11497 | — |
| 3492 | Bruneian History | — | 54420 | 54421 | 54422 |
| 3545 | Bulgarian History | 54678 | 51733 | 54679 | 13763 |
| 3326 | Burkina Faso. Upper Volta | — | 54083 | 54084 | 54089 |
| 3327 | Burundian History | — | 54278 | 54279 | 54286 |
| 13666 | Cambodian History | — | 54859 | 54860 | 54864 |
| 3328 | Cameroon Cameroun, Kamerun | — | 54288 | 54289 | 54292 |
| 3400 | Canadian History | 54622 | 54623 | 54624 | 50842 |
| 3337 | Central African Republic History | — | 54062 | 54063 | 54067 |
| 3338 | Chad Tchad | — | 54069 | 54070 | 54074 |
| 3428 | Chilean History | 54405 | 54406 | 54407 | 54408 |
| 3493 | Chinese History | 54637 | 51024 | 54638 | 50839 |
| 5542 | Colombian History | 54479 | 54480 | 54481 | 54482 |
| 3388 | Costa Rican History | 54366 | 54367 | 54368 | 54369 |
| 3546 | Croatian History | 54208 | 51743 | 54209 | 54210 |
| 3380 | Cuban History | 54196 | 54197 | 54198 | 54199 |
| 51930 | Cypriot History | — | 51935 | 54253 | 51944 |
| 3538 | Czechoslovak History | 54807 | 51722 | 54808 | 54809 |
| 3375 | DR Congo History | — | 54350 | 54351 | 54356 |
| 3580 | Danish History | 54697 | 51761 | 54698 | 13747 |
| 3381 | Dominican Republic History | 54153 | 54154 | 54155 | 54160 |
| 3579 | Dutch History | 54824 | 54825 | 54826 | 13871 |
| 5543 | Ecuadorian History | 54484 | 54485 | 54486 | 54487 |
| 3635 | Egyptian History | 52298 | 51078 | 54716 | 52302 |
| 3566 | English History | 50862 | 54691 | 54692 | 13659 |
| 10723 | Equatorial Guinean History | — | 54234 | 54235 | 54239 |
| 10657 | Eritrean History | — | — | 54509 | 54516 |
| 11216 | Estonian History | 54521 | 51842 | 54522 | 13948 |
| 3334 | Ethiopian History | — | 54294 | 54295 | — |
| 50852 | Falkland Islands History | — | — | — | — |
| 3581 | Finnish History | 54700 | 51805 | 54701 | 13942 |
| 50822 | French Guiana | — | — | 54247 | 54251 |
| 3523 | French History | 54796 | 51031 | 54797 | 52294 |
| 3340 | Gabon Gaboon, Gabun | — | 54076 | 54077 | 54081 |
| 3341 | Gambian History | — | 54091 | 54092 | 54096 |
| 3589 | Georgian History | 54454 | 54455 | 54456 | 13986 |
| 3524 | German History | 54799 | 51029 | 54800 | 13904 |
| 3535 | Greek History | — | — | 54802 | — |
| 3389 | Guatemalan History | 54371 | 54372 | 54373 | 54374 |
| 3343 | Guinean History | — | 54098 | 54099 | 54103 |
| 10366 | Guyanese History | — | 54216 | 54217 | 54218 |
| 3382 | Haitian History | 54162 | 54163 | 54164 | 54171 |
| 3390 | Honduran History | 54376 | 54377 | 54378 | 54379 |
| 3539 | Hungarian History | 54811 | 51618 | 54812 | 54813 |
| 3582 | Icelandic History | — | — | 54703 | 14019 |
| 3496 | Indian History | 54640 | 12724 | 54641 | 50847 |
| 3497 | Indonesian History | 54424 | 54425 | 54426 | 54427 |
| 3636 | Iranian History | 54718 | 54719 | 54720 | 50860 |
| 3637 | Iraqi History | 54722 | 54723 | 54724 | 54725 |
| 3606 | Irish History | 54708 | — | 54709 | 54710 |
| 3638 | Israel & Palestine | 54727 | 54728 | 54729 | 54730 |
| 3607 | Italian History | — | 54836 | 54837 | 13851 |
| 3383 | Jamaican History | — | 54201 | 54202 | 54206 |
| 3498 | Japanese History | 54643 | 54644 | 54645 | 50848 |
| 3499 | Jordan. Transjordan | 54647 | 54648 | 54649 | 54650 |
| 3590 | Kazakh History | 54458 | 54459 | 54460 | 13974 |
| 3345 | Kenyan History | — | 54180 | 54181 | 54186 |
| 3500 | Korean History | 10686 | 10571 | 54652 | 50845 |
| 51683 | Kosovo History | — | — | 54061 | — |
| 3591 | Kyrgyz History | — | 54462 | 54463 | 13976 |
| 13667 | Laotian History | — | 54866 | 54867 | 54872 |
| 11217 | Latvian History | 54524 | 51851 | 54525 | 13952 |
| 3501 | Lebanon Phenicia | 54654 | 54655 | 54656 | 54657 |
| 3346 | Lesotho. Basutoland | — | 54546 | 54547 | 54550 |
| 3347 | Liberian History | — | 54299 | 54300 | 54304 |
| 3348 | Libyan History | — | 54306 | 54307 | 54314 |
| 52000 | Liechtenstein History | — | 52004 | — | 52005 |
| 11221 | Lithuanian History | 54527 | 51860 | 54528 | 13955 |
| 3578 | Luxembourg History | — | 51925 | 54821 | 54822 |
| 3350 | Malawi. Nyasaland | — | 54552 | 54553 | 54558 |
| 3503 | Malaysian & Straits History | — | 54429 | 54430 | 54431 |
| 3351 | Mali History | — | 54105 | 54106 | 54111 |
| 3574 | Maltese History | 51905 | 51907 | 54815 | 54816 |
| 3352 | Mauritania | 54113 | 54114 | 54115 | 54119 |
| 3391 | Mexican History | 54617 | 54618 | 54619 | 54620 |
| 3568 | Modern Greek History | 52317 | 54694 | 54695 | 13741 |
| 3594 | Moldovan History | 51916 | 51917 | 54465 | 54466 |
| 52007 | Monaco History | — | — | — | — |
| 10535 | Mongolian History | — | 54753 | 54754 | 54755 |
| 10467 | Montenegrin History | 54220 | 54221 | 54222 | 54229 |
| 3353 | Mozambican History | — | 54560 | 54561 | 54567 |
| 13665 | Myanmar / Burma | 54849 | 54850 | 54851 | 54857 |
| 3354 | Namibia. South-West Africa | — | 54569 | 54570 | 54576 |
| 3504 | Nepalese History | 54769 | 54770 | 54771 | 54776 |
| 3681 | New Zealand History | 54742 | 54743 | 54744 | 54745 |
| 3392 | Nicaraguan History | 54381 | 54382 | 54383 | 54384 |
| 3356 | Nigerian History | 54129 | 54130 | 54131 | 54136 |
| 3355 | Nigerien History | — | 54121 | 54122 | 54127 |
| 51970 | North Macedonian History | 54263 | 51976 | 54264 | 51982 |
| 3583 | Norwegian History | 54705 | 51776 | 54706 | 13746 |
| 3505 | Pakistani History | 54659 | 54660 | 54661 | 54662 |
| 3393 | Panamanian History | 54386 | 54387 | 54388 | 54389 |
| 5544 | Paraguayan History | 54489 | 54490 | 54491 | 54492 |
| 3429 | Peruvian History | 54410 | 54411 | 54412 | 54413 |
| 3506 | Philippine History | 54433 | 54434 | 54435 | 54436 |
| 3610 | Polish History | 54839 | 51542 | 54840 | 50843 |
| 3611 | Portuguese History | 54842 | 51689 | 54843 | 13891 |
| 13830 | Puerto Rican History | — | — | 54241 | 54245 |
| 3548 | Romanian History | 54681 | 51624 | 54682 | 13762 |
| 3595 | Russian History | 54828 | 51032 | 54829 | 54830 |
| 3362 | Rwanda. Ruanda-Urundi | — | 54316 | 54317 | 54324 |
| 3425 | Saint Pierre and Miquelon | — | — | — | — |
| 3394 | Salvadoran History | 54391 | 54392 | 54393 | 54394 |
| 3530 | Scottish History | 54664 | 51602 | 54665 | 54666 |
| 3363 | Senegalese History | 54138 | 54139 | 54140 | 54144 |
| 3549 | Serbian History | 54212 | 51675 | 54213 | 54214 |
| 3364 | Sierra Leonean History | — | 54146 | 54147 | 54151 |
| 3507 | Singaporean History | — | 54438 | 54439 | 54440 |
| 3540 | Slovak History | 54671 | 54672 | 54673 | 14003 |
| 51945 | Slovenian History | 54255 | 54256 | 54257 | 51955 |
| 3365 | Somali & Somaliland History | — | 54326 | 54327 | 54333 |
| 3367 | South African History | 54578 | 54579 | 54580 | 54588 |
| 10281 | South Sudanese History | 54747 | 54748 | 54749 | 54751 |
| 3613 | Spanish History | 54845 | 54846 | 54847 | 13847 |
| 3510 | Sri Lankan History | 54778 | — | 54779 | 54784 |
| 3368 | Sudan. Anglo-Egyptian Sudan | 54590 | 54591 | 54592 | 54596 |
| 10589 | Surinamese History | — | — | 54231 | 54232 |
| 3584 | Swedish History | 54058 | 13935 | 54057 | 13749 |
| 3602 | Swiss History | 54832 | 51670 | 54833 | 54834 |
| 3639 | Syrian History | — | 54732 | 54733 | 54734 |
| 10699 | Taiwanese History | 54757 | 54758 | 54759 | 54760 |
| 3596 | Tajik History | — | — | 54468 | 13978 |
| 3369 | Tanzania & German E. Africa Hist. | — | 54335 | 54336 | 54340 |
| 3511 | Thai History | 54442 | 54443 | 54444 | 54445 |
| 10518 | Timorese History | — | — | 54506 | 54507 |
| 3370 | Togo. Togoland | — | 54342 | 54343 | 54348 |
| 3551 | Turkish History | 54684 | 54685 | 54686 | 54687 |
| 3597 | Turkmen History | — | — | 54470 | 13982 |
| 3402 | U.S. History | 9765 | 3416 | 3407 | 3418 |
| 3372 | Ugandan History | — | 54188 | 54189 | 54194 |
| 3603 | Ukrainian History | — | 54476 | 54477 | 13963 |
| 5545 | Uruguayan History | 54494 | 54495 | 54496 | 54497 |
| 3598 | Uzbek History | 54472 | 54473 | 54474 | 13984 |
| 3430 | Venezuelan History | 54415 | 54416 | 54417 | 54418 |
| 3513 | Vietnamese History | 54786 | 54787 | 54788 | 54794 |
| 3534 | Welsh History | — | — | 54668 | 54669 |
| 3374 | West Sahara | — | — | — | — |
| 3376 | Zambia. Northern Rhodesia | — | 54598 | 54599 | 54605 |
| 3377 | Zimbabwe. Southern Rhodesia | — | 54607 | 54608 | 54615 |

A dash means no separate supported subject/group was created at that level. Existing broader or nested country subjects remain available.

## Consolidated subject IDs

| Previous ID | Previous title | Destination ID | Destination title |
|---|---|---|---|
| 51744 | Croatian Foreign Relations History | 51743 | Political History & Foreign Relations |
| 51676 | Serbian Foreign Relations History | 51675 | Political History & Foreign Relations |
| 51950 | Slovenian Historical Studies | 51945 | Slovenian History |
| 51962 | Bosnian Historical Studies | 51957 | Bosnia and Herzegovina History |
| 51975 | North Macedonian Historical Studies | 51970 | North Macedonian History |
| 51898 | Belarusian Foreign Relations Hist. | 51897 | Political History & Foreign Relations |
| 13973 | General & Earlier History | 3590 | Kazakh History |
| 13975 | General & Earlier History | 3591 | Kyrgyz History |
| 51918 | Moldovan Foreign Relations History | 51917 | Political History & Foreign Relations |
| 13977 | General & Earlier History | 3596 | Tajik History |
| 13979 | General & Earlier History | 3597 | Turkmen History |
| 13983 | General & Earlier History | 3598 | Uzbek History |
| 13971 | Ukrainian Historical Studies | 3603 | Ukrainian History |
| 13968 | Other Ukrainian Town Histories | 13963 | Local History |
| 51843 | Estonian Foreign Relations History | 51842 | Political History & Foreign Relations |
| 13949 | Other Estonian Town Histories | 13948 | Local History |
| 51852 | Latvian Foreign Relations History | 51851 | Political History & Foreign Relations |
| 13953 | Other Latvian Town Histories | 13952 | Local History |
| 51861 | Lithuanian Foreign Relations Hist. | 51860 | Political History & Foreign Relations |
| 13956 | Other Lithuanian Town Histories | 13955 | Local History |
| 10441 | Korean Foreign Relations History | 10571 | Political History & Foreign Relations |
| 14000 | Slovak Historical Studies | 3540 | Slovak History |
| 14007 | Albanian Historical Studies | 3544 | Albanian History |
| 51830 | Albanian Foreign Relations History | 51829 | Political History & Foreign Relations |
| 14010 | Other Albanian Town Histories | 14008 | Local History |
| 13917 | Bulgarian Historical Studies | 3545 | Bulgarian History |
| 13764 | Cities & Towns | 13763 | Local History |
| 13895 | Romanian Historical Studies | 3548 | Romanian History |
| 13897 | Regions & Major Cities | 13762 | Local History |
| 13907 | Turkish Historical Studies | 3551 | Turkish History |
| 13743 | Other Modern Greek Town Histories | 13741 | Local History |
| 13929 | Danish Historical Studies | 3580 | Danish History |
| 51762 | Danish Foreign Relations History | 51761 | Political History & Foreign Relations |
| 13748 | Cities & Towns | 13747 | Local History |
| 13939 | Finnish Historical Studies | 3581 | Finnish History |
| 51806 | Finnish Foreign Relations History | 51805 | Political History & Foreign Relations |
| 13944 | Other Finnish Town Histories | 13942 | Local History |
| 14018 | Icelandic Historical Studies | 3582 | Icelandic History |
| 14020 | Cities & Towns | 14019 | Local History |
| 13932 | Norwegian Historical Studies | 3583 | Norwegian History |
| 54060 | Other Cities & Towns | 54059 | Other Local History |
| 13750 | Cities & Towns | 13749 | Local History |
| 3410 | U.S. Foreign Relations History | 3416 | Political History & Foreign Relations |
| 8368 | U.S. State & County History | 3418 | Local History |
| 52289 | General & Cross-period French History | 3523 | French History |
| 13900 | German Historical Studies | 3524 | German History |
| 13883 | Czechoslovak Historical Studies | 3538 | Czechoslovak History |
| 51723 | Czechoslovak Foreign Relations Hist. | 51722 | Political History & Foreign Relations |
| 51906 | Maltese Naval History | 51905 | Military & Naval History |
| 51908 | Maltese Foreign Relations History | 51907 | Political History & Foreign Relations |
| 13922 | Belgian Historical Studies | 3577 | Belgian History |
| 13753 | Other Belgian Town Histories | 13751 | Local History |
| 52003 | Liechtenstein General History | 52000 | Liechtenstein History |

## Fallback representation

Other Local History uses ranges rather than exact class selectors, so it does not outrank named places. Redundant synthetic upper .Z endpoints are removed where they would inflate fallback specificity. Existing named-place selectors and ranges are unchanged.

Usage counts were recomputed from the OpenLibrary 2026-07-31 cache (normalization version 2). Counts represent classification rows, not unique books.
