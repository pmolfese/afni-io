StateDef = smoothwm
StateDef = pial
NewSurface
	SurfaceType = FreeSurfer
	SurfaceName = lh.smoothwm.asc
	SurfaceState = smoothwm
	MappingRef = SAME
NewSurface
	SurfaceName = lh.pial.asc
	SurfaceState = pial
	MappingRef = lh.smoothwm.asc
